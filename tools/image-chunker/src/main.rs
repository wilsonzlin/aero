use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use aero_storage::{DiskFormat, DiskImage, FileBackend, VirtualDisk, SECTOR_SIZE};
use anyhow::{anyhow, bail, Context, Result};
use aws_config::meta::region::RegionProviderChain;
use aws_config::BehaviorVersion;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use aws_types::region::Region;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH,
    CONTENT_RANGE, RANGE,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

const MANIFEST_SCHEMA: &str = "aero.chunked-disk-image.v1";
const CHUNK_MIME_TYPE: &str = "application/octet-stream";
const JSON_MIME_TYPE: &str = "application/json";
const LATEST_SCHEMA: &str = "aero.chunked-disk-image.latest.v1";
const DEFAULT_CACHE_CONTROL_CHUNKS: &str = "public, max-age=31536000, immutable, no-transform";
const DEFAULT_CACHE_CONTROL_MANIFEST: &str = "public, max-age=31536000, immutable, no-transform";
const DEFAULT_CACHE_CONTROL_LATEST: &str = "public, max-age=60, no-transform";
// For compatibility with Aero's clients and tooling (and to prevent CDNs from applying transparent
// compression), publish all chunked artifacts with `Content-Encoding: identity`.
const IDENTITY_CONTENT_ENCODING: &str = "identity";
// Browsers automatically send a non-identity Accept-Encoding (and scripts cannot override it).
// Use a browser-like value so HTTP verification catches any CDN/object-store compression that
// would break byte-addressed disk reads.
const BROWSER_ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";
const DEFAULT_CHUNK_SIZE_BYTES: u64 = 4 * 1024 * 1024;
// Defensive bounds to avoid producing or verifying manifests that the reference clients will
// reject. Keep aligned with:
// - `web/src/storage/remote_chunked_disk.ts`
// - `crates/aero-storage/src/chunked_streaming.rs`
const MAX_CHUNK_SIZE_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB
const MAX_COMPAT_CHUNK_COUNT: u64 = 500_000;
// Manifest JSON size cap (defensive). This mirrors the reference runtime clients.
const MAX_MANIFEST_JSON_BYTES: usize = 64 * 1024 * 1024; // 64 MiB
const DEFAULT_CONCURRENCY: usize = 8;
const DEFAULT_RETRIES: usize = 5;
const CHUNK_INDEX_WIDTH: usize = 8;
const MAX_CHUNKS: u64 = 100_000_000;

#[derive(Debug, Parser)]
#[command(name = "aero-image-chunker", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Chunk a disk image and publish it to an S3-compatible object store.
    Publish(PublishArgs),
    /// Verify a published chunked disk image (manifest + chunks) for integrity and correctness.
    Verify(VerifyArgs),
}

#[derive(Debug, Parser)]
struct PublishArgs {
    /// Path to an input disk image file.
    #[arg(long)]
    file: PathBuf,

    /// Input disk image format.
    ///
    /// Chunks always contain the *expanded* guest-visible disk byte stream (a raw "disk view").
    /// For container formats like qcow2/vhd/aerosparse, this means the output chunks are not the
    /// same as the input file bytes.
    #[arg(long, value_enum, default_value_t = InputFormat::Auto)]
    format: InputFormat,

    /// Destination bucket name.
    #[arg(long)]
    bucket: String,

    /// Key prefix to upload under.
    ///
    /// Recommended layout:
    /// - Versioned artifacts: `images/<imageId>/<version>/...`
    /// - Optional latest pointer: `images/<imageId>/latest.json`
    ///
    /// When `--image-version` is supplied (or `--compute-version sha256` is used), `--prefix` may
    /// point at the image root (e.g. `images/<imageId>/`) and the tool will append
    /// `/<version>/`.
    #[arg(long)]
    prefix: String,

    /// Image identifier written into the manifest (recommended stable id, e.g. `win7-sp1-x64`).
    ///
    /// If omitted, it is inferred from `--prefix`.
    #[arg(long)]
    image_id: Option<String>,

    /// Version identifier written into the manifest (recommended: content hash, e.g. `sha256-...`).
    ///
    /// If omitted:
    /// - with `--compute-version none` (default): inferred from `--prefix` by taking the last
    ///   non-empty path segment.
    /// - with `--compute-version sha256`: computed as `sha256-<digest>` over the entire disk image
    ///   content.
    #[arg(long)]
    image_version: Option<String>,

    /// Compute a full-image version identifier from the entire disk image content.
    ///
    /// When set to `sha256`, the tool computes `sha256-<digest>` over the entire disk image
    /// content before uploading (this requires reading the expanded disk bytes twice: hash, then upload).
    ///
    /// If `--image-version` is omitted, the computed hash becomes the manifest `version` and is
    /// used for the versioned upload prefix.
    ///
    /// If `--image-version` is also provided, it must match the computed hash.
    #[arg(long, value_enum, default_value_t = ComputeVersion::None)]
    compute_version: ComputeVersion,

    /// Upload `images/<imageId>/latest.json` (short TTL) pointing at the newly published manifest.
    ///
    /// This is intended for public/demo images where you want a stable "latest" pointer in
    /// addition to immutable versioned manifests.
    #[arg(long, default_value_t = false)]
    publish_latest: bool,

    /// Cache-Control value to set on chunk objects (`chunks/*.bin`).
    #[arg(long, default_value = DEFAULT_CACHE_CONTROL_CHUNKS)]
    cache_control_chunks: String,

    /// Cache-Control value to set on JSON objects (`manifest.json`, `meta.json`).
    #[arg(long, default_value = DEFAULT_CACHE_CONTROL_MANIFEST)]
    cache_control_manifest: String,

    /// Cache-Control value to set on `latest.json`.
    #[arg(long, default_value = DEFAULT_CACHE_CONTROL_LATEST)]
    cache_control_latest: String,

    /// Chunk size in bytes (must be a multiple of 512; max 64 MiB).
    #[arg(long, default_value_t = DEFAULT_CHUNK_SIZE_BYTES)]
    chunk_size: u64,

    /// Per-chunk checksum algorithm.
    #[arg(long, value_enum, default_value_t = ChecksumAlgorithm::Sha256)]
    checksum: ChecksumAlgorithm,

    /// Custom S3 endpoint URL (e.g. http://localhost:9000 for MinIO).
    #[arg(long)]
    endpoint: Option<String>,

    /// Use path-style addressing (required for some S3-compatible endpoints).
    #[arg(long, default_value_t = false)]
    force_path_style: bool,

    /// AWS region.
    #[arg(long, default_value = "us-east-1")]
    region: String,

    /// Number of parallel uploads.
    #[arg(long, default_value_t = DEFAULT_CONCURRENCY)]
    concurrency: usize,

    /// Max attempts per chunk upload.
    #[arg(long, default_value_t = DEFAULT_RETRIES)]
    retries: usize,

    /// Do not upload `meta.json`.
    #[arg(long, default_value_t = false)]
    no_meta: bool,
}

#[derive(Debug, Parser)]
struct VerifyArgs {
    /// URL to a `manifest.json` (HTTP GET).
    #[arg(long, conflicts_with_all = ["manifest_file", "bucket"])]
    manifest_url: Option<String>,

    /// Local path to a `manifest.json`.
    #[arg(long, conflicts_with_all = ["manifest_url", "bucket"])]
    manifest_file: Option<PathBuf>,

    /// Extra HTTP headers to include when fetching the manifest and chunk objects.
    ///
    /// Repeatable; format: `Header-Name: value`
    #[arg(long, value_name = "HEADER")]
    header: Vec<String>,

    /// Destination bucket name (S3 mode).
    #[arg(long, conflicts_with_all = ["manifest_url", "manifest_file"])]
    bucket: Option<String>,

    /// Prefix of a versioned image (e.g. `images/<imageId>/<version>/`) or an image root
    /// (e.g. `images/<imageId>/`) when combined with `--image-version` (S3 mode).
    ///
    /// The tool will fetch `<prefix>/manifest.json` (versioned prefix) or
    /// `<prefix>/<imageVersion>/manifest.json` (image root + `--image-version`).
    ///
    /// If `<prefix>/manifest.json` is not found and `--image-version` is not provided, the tool
    /// will attempt to resolve `latest.json` under the given prefix and verify the referenced
    /// versioned manifest instead.
    #[arg(long, conflicts_with = "manifest_key")]
    prefix: Option<String>,

    /// Explicit object key of `manifest.json` to verify (S3 mode).
    #[arg(long, conflicts_with = "prefix")]
    manifest_key: Option<String>,

    /// Expected image identifier (validated against the manifest).
    #[arg(long)]
    image_id: Option<String>,

    /// Expected version identifier (validated against the manifest).
    #[arg(long)]
    image_version: Option<String>,

    /// Custom S3 endpoint URL (e.g. http://localhost:9000 for MinIO) (S3 mode).
    #[arg(long)]
    endpoint: Option<String>,

    /// Use path-style addressing (required for some S3-compatible endpoints) (S3 mode).
    #[arg(long, default_value_t = false)]
    force_path_style: bool,

    /// AWS region (S3 mode).
    #[arg(long, default_value = "us-east-1")]
    region: String,

    /// Number of parallel chunk downloads.
    #[arg(long, default_value_t = DEFAULT_CONCURRENCY)]
    concurrency: usize,

    /// Max attempts per object download.
    #[arg(long, default_value_t = DEFAULT_RETRIES)]
    retries: usize,

    /// Safety guard for manifest-reported chunk counts.
    #[arg(
        long,
        default_value_t = MAX_COMPAT_CHUNK_COUNT,
        value_parser = clap::value_parser!(u64).range(1..=MAX_CHUNKS)
    )]
    max_chunks: u64,

    /// Only verify N random chunks plus the final chunk (useful for quick smoke tests).
    #[arg(long)]
    chunk_sample: Option<u64>,

    /// Seed for `--chunk-sample` randomization (enables deterministic sampling for CI).
    #[arg(long, requires = "chunk_sample")]
    chunk_sample_seed: Option<u64>,
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum InputFormat {
    /// Interpret the input file bytes directly as the guest-visible disk bytes.
    Raw,
    /// Interpret the input as an Aero sparse disk image (`AEROSPAR`).
    #[value(name = "aerosparse", alias = "aerospar")]
    AeroSparse,
    /// Interpret the input as a QCOW2 v2/v3 disk image.
    Qcow2,
    /// Interpret the input as a VHD disk image (fixed/dynamic).
    Vhd,
    /// Auto-detect the disk format from magic values.
    Auto,
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum ChecksumAlgorithm {
    None,
    Sha256,
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum ComputeVersion {
    None,
    Sha256,
}

impl ChecksumAlgorithm {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Sha256 => "sha256",
        }
    }
}

#[derive(Debug)]
struct ChunkJob {
    index: u64,
    bytes: Bytes,
}

#[derive(Debug)]
struct ChunkResult {
    index: u64,
    sha256: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ManifestV1 {
    schema: String,
    image_id: String,
    version: String,
    mime_type: String,
    total_size: u64,
    chunk_size: u64,
    chunk_count: u64,
    chunk_index_width: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chunks: Option<Vec<ManifestChunkV1>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ManifestChunkV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    created_at: DateTime<Utc>,
    original_filename: String,
    total_size: u64,
    chunk_size: u64,
    chunk_count: u64,
    checksum_algorithm: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LatestV1 {
    schema: String,
    image_id: String,
    version: String,
    manifest_key: String,
}

fn tokio_worker_threads_from_env() -> Option<usize> {
    let raw = match std::env::var("AERO_TOKIO_WORKER_THREADS") {
        Ok(v) => v,
        Err(_) => return None,
    };
    match raw.parse::<usize>() {
        Ok(n) if n > 0 => Some(n),
        _ => {
            eprintln!(
                "warning: invalid AERO_TOKIO_WORKER_THREADS value: {raw:?} (expected positive integer); using Tokio default"
            );
            None
        }
    }
}

fn build_tokio_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(n) = tokio_worker_threads_from_env() {
        builder.worker_threads(n);
    }
    builder.enable_all().build()
}

fn main() -> Result<()> {
    build_tokio_runtime()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Publish(args) => publish(args).await,
        Commands::Verify(args) => verify(args).await,
    }
}

async fn publish(args: PublishArgs) -> Result<()> {
    validate_args(&args)?;

    let prefix = normalize_prefix(&args.prefix);
    let (input_format, total_size) =
        inspect_input_disk(&args.file, args.format).context("open input disk")?;
    let sector = SECTOR_SIZE as u64;
    if total_size == 0 {
        bail!("virtual disk size must be > 0");
    }
    if !total_size.is_multiple_of(sector) {
        bail!("virtual disk size {total_size} is not a multiple of {sector} bytes");
    }
    let chunk_count = chunk_count(total_size, args.chunk_size);
    if chunk_count > MAX_COMPAT_CHUNK_COUNT {
        bail!(
            "image requires {chunk_count} chunks which exceeds the current compatibility limit of {MAX_COMPAT_CHUNK_COUNT}; increase --chunk-size to reduce chunkCount"
        );
    }
    if chunk_count > MAX_CHUNKS {
        bail!(
            "image requires {chunk_count} chunks which exceeds the current limit of {MAX_CHUNKS} (chunk size too small?)"
        );
    }

    let computed_version = match args.compute_version {
        ComputeVersion::None => None,
        ComputeVersion::Sha256 => {
            eprintln!(
                "Computing full-image SHA-256 version from {}...",
                args.file.display()
            );
            Some(compute_image_version_sha256(&args.file, args.format).await?)
        }
    };

    let destination = resolve_publish_destination(&args, &prefix, computed_version.as_deref())?;
    let image_id = destination.image_id.clone();
    let version = destination.version.clone();
    let version_prefix = destination.version_prefix.clone();
    let manifest_key = manifest_object_key(&version_prefix);

    let s3 = build_s3_client(
        args.endpoint.as_deref(),
        args.force_path_style,
        &args.region,
    )
    .await?;

    eprintln!(
        "Publishing {}\n  input format: {:?}\n  imageId: {}\n  version: {}\n  total size: {} bytes\n  chunk size: {} bytes\n  chunk count: {}\n  destination: s3://{}/{}",
        args.file.display(),
        input_format,
        image_id,
        version,
        total_size,
        args.chunk_size,
        chunk_count,
        args.bucket,
        version_prefix
    );

    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {bytes}/{total_bytes} {msg} ({eta})",
        )?
        .progress_chars("##-"),
    );
    pb.set_message(format!("0/{chunk_count} chunks"));

    let chunks_uploaded = Arc::new(AtomicU64::new(0));

    // Keep the in-flight chunk buffer count bounded to cap memory: each worker owns at most one
    // chunk at a time, and this queue limits producer read-ahead.
    let (work_tx, work_rx) = async_channel::bounded::<ChunkJob>(args.concurrency);
    let (result_tx, result_rx) = tokio::sync::mpsc::channel::<ChunkResult>(args.concurrency);

    // Drain chunk results concurrently so we don't buffer `ChunkResult` messages for the entire
    // disk before constructing the manifest.
    let checksum = args.checksum;
    let result_collector = tokio::spawn(async move {
        let mut sha256_by_index: Vec<Option<String>> =
            if matches!(checksum, ChecksumAlgorithm::Sha256) {
                vec![None; chunk_count as usize]
            } else {
                Vec::new()
            };

        let mut result_rx = result_rx;
        let mut collector_err: Option<anyhow::Error> = None;
        while let Some(result) = result_rx.recv().await {
            if matches!(checksum, ChecksumAlgorithm::Sha256) {
                match usize::try_from(result.index) {
                    Ok(idx) => {
                        if let Some(slot) = sha256_by_index.get_mut(idx) {
                            *slot = result.sha256;
                        } else if collector_err.is_none() {
                            collector_err = Some(anyhow!(
                                "chunk index {idx} is out of bounds (sha256 vector len={})",
                                sha256_by_index.len()
                            ));
                        }
                    }
                    Err(_) => {
                        if collector_err.is_none() {
                            collector_err = Some(anyhow!(
                                "chunk index {} does not fit into usize",
                                result.index
                            ));
                        }
                    }
                }
            }
        }

        match collector_err {
            Some(err) => Err(err),
            None => Ok(sha256_by_index),
        }
    });

    let mut workers = Vec::with_capacity(args.concurrency);
    for _ in 0..args.concurrency {
        let work_rx = work_rx.clone();
        let result_tx = result_tx.clone();
        let s3 = s3.clone();
        let bucket = args.bucket.clone();
        let prefix = version_prefix.clone();
        let cache_control_chunks = args.cache_control_chunks.clone();
        let checksum = args.checksum;
        let retries = args.retries;
        let pb = pb.clone();
        let chunks_uploaded = Arc::clone(&chunks_uploaded);
        workers.push(tokio::spawn(async move {
            worker_loop(
                work_rx,
                result_tx,
                s3,
                bucket,
                prefix,
                cache_control_chunks,
                checksum,
                retries,
                pb,
                chunks_uploaded,
                chunk_count,
            )
            .await
        }));
    }
    drop(result_tx);
    // Drop the unused receiver handle so if all workers exit early (e.g. upload failures),
    // the producer will observe the channel closing instead of deadlocking on a full queue.
    drop(work_rx);

    let reader_path = args.file.clone();
    let reader_format = args.format;
    let reader_chunk_size = args.chunk_size;
    let reader_chunk_count = chunk_count;
    let reader_total_size = total_size;
    let reader_handle = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut disk = open_input_disk(&reader_path, reader_format)?;

        for index in 0..reader_chunk_count {
            let offset = index
                .checked_mul(reader_chunk_size)
                .ok_or_else(|| anyhow!("chunk offset overflows u64"))?;
            let remaining = reader_total_size.saturating_sub(offset);
            let expected = std::cmp::min(reader_chunk_size, remaining);
            let expected_usize: usize = expected
                .try_into()
                .map_err(|_| anyhow!("chunk size {expected} does not fit into usize"))?;
            let mut buf = vec![0u8; expected_usize];
            disk.read_at(offset, &mut buf)
                .with_context(|| format!("read chunk {index} at offset {offset}"))?;

            let bytes = Bytes::from(buf);
            work_tx
                .send_blocking(ChunkJob { index, bytes })
                .map_err(|err| anyhow!("internal worker channel closed unexpectedly: {err}"))?;
        }
        Ok(())
    });

    let reader_result: Result<()> = match reader_handle.await {
        Ok(res) => res,
        Err(err) => Err(anyhow!("disk reader panicked: {err}")),
    };

    // Always await all worker tasks so we don't leave uploads running in the background if one
    // worker errors.
    let mut worker_result: Result<()> = Ok(());
    for handle in workers {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                if worker_result.is_ok() {
                    worker_result = Err(err);
                }
            }
            Err(err) => {
                if worker_result.is_ok() {
                    worker_result = Err(anyhow!("upload worker panicked: {err}"));
                }
            }
        }
    }

    let sha256_by_index = result_collector
        .await
        .map_err(|err| anyhow!("result collector panicked: {err}"))??;

    reader_result?;
    worker_result?;

    pb.finish_with_message(format!("{chunk_count}/{chunk_count} chunks"));

    let manifest = build_manifest_v1(
        total_size,
        args.chunk_size,
        &image_id,
        &version,
        args.checksum,
        &sha256_by_index,
    )?;
    upload_json_object(
        &s3,
        &args.bucket,
        &manifest_key,
        &manifest,
        &args.cache_control_manifest,
        args.retries,
    )
    .await?;

    if !args.no_meta {
        let meta = Meta {
            created_at: Utc::now(),
            original_filename: args
                .file
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown")
                .to_string(),
            total_size,
            chunk_size: args.chunk_size,
            chunk_count,
            checksum_algorithm: args.checksum.as_str().to_string(),
        };
        upload_json_object(
            &s3,
            &args.bucket,
            &meta_object_key(&version_prefix),
            &meta,
            &args.cache_control_manifest,
            args.retries,
        )
        .await?;
    }

    if args.publish_latest {
        let latest = LatestV1 {
            schema: LATEST_SCHEMA.to_string(),
            image_id: image_id.clone(),
            version: version.clone(),
            manifest_key: manifest_key.clone(),
        };
        upload_json_object(
            &s3,
            &args.bucket,
            &latest_object_key(&destination.image_root_prefix),
            &latest,
            &args.cache_control_latest,
            args.retries,
        )
        .await?;
    }

    eprintln!("Done.");
    Ok(())
}

async fn verify(args: VerifyArgs) -> Result<()> {
    validate_verify_args(&args)?;

    if args.manifest_url.is_some() || args.manifest_file.is_some() {
        verify_http_or_file(&args).await
    } else {
        verify_s3(&args).await
    }
}

#[derive(Clone, Debug)]
enum VerifyHttpSource {
    File {
        base_dir: PathBuf,
    },
    Url {
        manifest_url: reqwest::Url,
        client: reqwest::Client,
        head_supported: Arc<AtomicBool>,
        range_supported: Arc<AtomicBool>,
    },
}

#[derive(Debug)]
struct HttpStatusFailure {
    url: reqwest::Url,
    status: reqwest::StatusCode,
}

impl std::fmt::Display for HttpStatusFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GET {} failed with HTTP {}", self.url, self.status)
    }
}

impl std::error::Error for HttpStatusFailure {}

async fn verify_http_or_file(args: &VerifyArgs) -> Result<()> {
    let started_at = Instant::now();

    let (manifest_bytes, source) = if let Some(url) = &args.manifest_url {
        let manifest_url: reqwest::Url = url.parse().context("parse --manifest-url")?;
        let client = build_reqwest_client(&args.header)?;
        let bytes = download_http_bytes_with_retry(
            &client,
            manifest_url.clone(),
            args.retries,
            MAX_MANIFEST_JSON_BYTES,
        )
        .await
        .context("download manifest.json")?;
        (
            bytes,
            VerifyHttpSource::Url {
                manifest_url,
                client,
                head_supported: Arc::new(AtomicBool::new(true)),
                range_supported: Arc::new(AtomicBool::new(true)),
            },
        )
    } else if let Some(path) = &args.manifest_file {
        let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let meta = tokio::fs::metadata(path)
            .await
            .with_context(|| format!("stat {}", path.display()))?;
        let size: usize = meta.len().try_into().map_err(|_| {
            anyhow!(
                "manifest file {} is too large to fit in memory (len={})",
                path.display(),
                meta.len()
            )
        })?;
        if size > MAX_MANIFEST_JSON_BYTES {
            bail!(
                "manifest file {} is too large (max {MAX_MANIFEST_JSON_BYTES} bytes, got {size})",
                path.display()
            );
        }
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        (bytes, VerifyHttpSource::File { base_dir })
    } else {
        bail!("either --manifest-url or --manifest-file is required");
    };

    let manifest: ManifestV1 =
        serde_json::from_slice(&manifest_bytes).context("parse manifest.json")?;
    validate_manifest_v1(&manifest, args.max_chunks)?;
    validate_manifest_identity(&manifest, args)?;

    if manifest.mime_type != CHUNK_MIME_TYPE {
        eprintln!(
            "Warning: manifest mimeType is '{}', expected '{}' (this does not affect integrity verification but may indicate incorrect publishing metadata).",
            manifest.mime_type, CHUNK_MIME_TYPE
        );
    }

    eprintln!(
        "Verifying imageId={} version={} chunkSize={} chunkCount={} totalSize={}",
        manifest.image_id,
        manifest.version,
        manifest.chunk_size,
        manifest.chunk_count,
        manifest.total_size
    );

    verify_optional_meta_http_or_file(&source, &manifest, args.retries).await?;

    let manifest = Arc::new(manifest);
    let chunk_count = manifest.chunk_count;

    let verify_all = match args.chunk_sample {
        None => true,
        Some(n) => n.saturating_add(1) >= chunk_count,
    };
    let indices: Option<Vec<u64>> = if verify_all {
        None
    } else {
        let mut rng = match args.chunk_sample_seed {
            Some(seed) => fastrand::Rng::with_seed(seed),
            None => fastrand::Rng::new(),
        };
        Some(select_sampled_chunk_indices(
            chunk_count,
            args.chunk_sample.unwrap(),
            &mut rng,
        )?)
    };
    let total_chunks_to_verify = indices
        .as_ref()
        .map(|v| v.len() as u64)
        .unwrap_or(chunk_count);

    let total_bytes_to_verify = match &indices {
        None => manifest.total_size,
        Some(indices) => {
            let mut total: u64 = 0;
            for &idx in indices {
                total = total
                    .checked_add(expected_chunk_size(manifest.as_ref(), idx)?)
                    .ok_or_else(|| anyhow!("total bytes to verify overflows u64"))?;
            }
            total
        }
    };

    let work_cap = args.concurrency.saturating_mul(2).max(1);
    let (work_tx, work_rx) = async_channel::bounded::<u64>(work_cap);
    let (err_tx, mut err_rx) = tokio::sync::mpsc::unbounded_channel::<anyhow::Error>();

    let cancelled = Arc::new(AtomicBool::new(false));
    let chunks_checked = Arc::new(AtomicU64::new(0));
    let bytes_checked = Arc::new(AtomicU64::new(0));

    let mut workers = Vec::with_capacity(args.concurrency);
    let retries = args.retries;
    for _ in 0..args.concurrency {
        let work_rx = work_rx.clone();
        let err_tx = err_tx.clone();
        let source = source.clone();
        let manifest = Arc::clone(&manifest);
        let cancelled = Arc::clone(&cancelled);
        let chunks_checked = Arc::clone(&chunks_checked);
        let bytes_checked = Arc::clone(&bytes_checked);
        workers.push(tokio::spawn(async move {
            while let Ok(index) = work_rx.recv().await {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }

                match verify_http_chunk(index, &manifest, &source, retries).await {
                    Ok(bytes) => {
                        bytes_checked.fetch_add(bytes, Ordering::SeqCst);
                        chunks_checked.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(err) => {
                        cancelled.store(true, Ordering::SeqCst);
                        let _ = err_tx.send(err);
                        break;
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        }));
    }
    drop(err_tx);
    // Drop the unused receiver handle so if all workers exit early (e.g. due to an internal error),
    // the producer will observe the channel closing instead of deadlocking on a full queue.
    drop(work_rx);

    let send_jobs = async {
        if let Some(indices) = indices {
            for index in indices {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                tokio::select! {
                    res = work_tx.send(index) => {
                        res.map_err(|err| anyhow!("internal worker channel closed unexpectedly: {err}"))?;
                    }
                    Some(err) = err_rx.recv() => {
                        cancelled.store(true, Ordering::SeqCst);
                        return Err(err);
                    }
                }
            }
        } else {
            for index in 0..chunk_count {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                tokio::select! {
                    res = work_tx.send(index) => {
                        res.map_err(|err| anyhow!("internal worker channel closed unexpectedly: {err}"))?;
                    }
                    Some(err) = err_rx.recv() => {
                        cancelled.store(true, Ordering::SeqCst);
                        return Err(err);
                    }
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    let send_result = send_jobs.await;
    drop(work_tx);

    if let Err(err) = send_result {
        for handle in &workers {
            handle.abort();
        }
        for handle in workers {
            let _ = handle.await;
        }
        return Err(err);
    }

    if let Some(err) = err_rx.recv().await {
        for handle in &workers {
            handle.abort();
        }
        for handle in workers {
            let _ = handle.await;
        }
        return Err(err);
    }

    for handle in workers {
        handle
            .await
            .map_err(|err| anyhow!("verify worker panicked: {err}"))??;
    }

    let checked = chunks_checked.load(Ordering::SeqCst);
    let checked_bytes = bytes_checked.load(Ordering::SeqCst);
    if checked != total_chunks_to_verify {
        bail!("internal error: only checked {checked}/{total_chunks_to_verify} chunks");
    }
    if checked_bytes != total_bytes_to_verify {
        bail!("internal error: only checked {checked_bytes}/{total_bytes_to_verify} bytes");
    }

    let elapsed = started_at.elapsed();
    println!(
        "Verified {checked}/{total_chunks_to_verify} chunks ({checked_bytes} bytes) in {elapsed:.2?}"
    );
    Ok(())
}

async fn verify_optional_meta_http_or_file(
    source: &VerifyHttpSource,
    manifest: &ManifestV1,
    retries: usize,
) -> Result<()> {
    match source {
        VerifyHttpSource::File { base_dir } => {
            let meta_path = base_dir.join("meta.json");
            match tokio::fs::metadata(&meta_path).await {
                Ok(meta) => {
                    let size: usize = meta.len().try_into().map_err(|_| {
                        anyhow!(
                            "meta.json at {} is too large to fit in memory (len={})",
                            meta_path.display(),
                            meta.len()
                        )
                    })?;
                    if size > MAX_MANIFEST_JSON_BYTES {
                        bail!(
                            "meta.json at {} is too large (max {MAX_MANIFEST_JSON_BYTES} bytes, got {size})",
                            meta_path.display()
                        );
                    }
                    let bytes = tokio::fs::read(&meta_path)
                        .await
                        .with_context(|| format!("read {}", meta_path.display()))?;
                    let meta: Meta = serde_json::from_slice(&bytes)
                        .with_context(|| format!("parse meta.json at {}", meta_path.display()))?;
                    validate_meta_matches_manifest(&meta, manifest)?;
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    eprintln!(
                        "Note: {} not found; skipping meta.json validation.",
                        meta_path.display()
                    );
                }
                Err(err) => {
                    return Err(err).with_context(|| format!("stat {}", meta_path.display()));
                }
            };
        }
        VerifyHttpSource::Url {
            manifest_url,
            client,
            ..
        } => {
            let mut meta_url = manifest_url
                .join("meta.json")
                .with_context(|| format!("resolve meta.json relative to {manifest_url}"))?;
            // Preserve querystring auth material from the manifest URL (e.g. signed URLs).
            meta_url.set_query(manifest_url.query());
            meta_url.set_fragment(None);
            match download_http_bytes_optional_with_retry(
                client,
                meta_url.clone(),
                retries,
                MAX_MANIFEST_JSON_BYTES,
            )
            .await?
            {
                None => {
                    eprintln!("Note: {meta_url} not found; skipping meta.json validation.");
                }
                Some(bytes) => {
                    let meta: Meta =
                        serde_json::from_slice(&bytes).context("parse meta.json from HTTP")?;
                    validate_meta_matches_manifest(&meta, manifest)?;
                }
            }
        }
    }
    Ok(())
}

fn validate_manifest_identity(manifest: &ManifestV1, args: &VerifyArgs) -> Result<()> {
    if let Some(expected) = &args.image_id {
        if manifest.image_id != *expected {
            bail!(
                "manifest imageId mismatch: expected '{expected}', got '{}'",
                manifest.image_id
            );
        }
    }
    if let Some(expected) = &args.image_version {
        if manifest.version != *expected {
            bail!(
                "manifest version mismatch: expected '{expected}', got '{}'",
                manifest.version
            );
        }
    }
    Ok(())
}

fn build_reqwest_client(headers: &[String]) -> Result<reqwest::Client> {
    let mut header_map = HeaderMap::new();
    for raw in headers {
        let (name, value) = parse_header(raw)?;
        header_map.insert(name, value);
    }
    // Match browser semantics: scripts cannot force `Accept-Encoding: identity`, so use a
    // browser-like value and fail fast if the server tries to apply compression transforms.
    header_map.insert(
        ACCEPT_ENCODING,
        HeaderValue::from_static(BROWSER_ACCEPT_ENCODING),
    );
    reqwest::Client::builder()
        .default_headers(header_map)
        .build()
        .context("build http client")
}

fn parse_header(raw: &str) -> Result<(HeaderName, HeaderValue)> {
    let (name, value) = raw
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid header {raw:?} (expected 'Header-Name: value')"))?;
    let name = HeaderName::from_bytes(name.trim().as_bytes())
        .with_context(|| format!("invalid header name in {raw:?}"))?;
    let value = HeaderValue::from_str(value.trim())
        .with_context(|| format!("invalid header value in {raw:?}"))?;
    Ok((name, value))
}

fn parse_content_range_total(value: &str) -> Option<u64> {
    // Example: `bytes 0-0/1024` or `bytes 0-0/*`.
    let value = value.trim();
    let (unit, rest) = value.split_once(' ')?;
    if unit != "bytes" {
        return None;
    }
    let (_range, total) = rest.split_once('/')?;
    let total = total.trim();
    if total == "*" {
        return None;
    }
    total.parse::<u64>().ok()
}

fn is_retryable_http_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
}

fn is_retryable_http_error(err: &anyhow::Error) -> bool {
    // Treat deterministic integrity failures as non-retryable.
    for cause in err.chain() {
        let msg = cause.to_string();
        if msg.contains("size mismatch") || msg.contains("sha256 mismatch") {
            return false;
        }
        if msg.contains("unexpected Content-Encoding") {
            return false;
        }
        if msg.contains("response too large") {
            return false;
        }

        if let Some(status) = cause.downcast_ref::<HttpStatusFailure>() {
            let code = status.status;
            if code == reqwest::StatusCode::NOT_FOUND {
                return false;
            }
            if code.is_client_error()
                && code != reqwest::StatusCode::TOO_MANY_REQUESTS
                && code != reqwest::StatusCode::REQUEST_TIMEOUT
            {
                return false;
            }
            return is_retryable_http_status(code);
        }
    }

    true
}

async fn download_http_bytes_with_retry(
    client: &reqwest::Client,
    url: reqwest::Url,
    retries: usize,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;

        let resp_result = client.get(url.clone()).send().await;
        match resp_result {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let result = read_http_response_bytes_with_limit(resp, max_bytes)
                        .await
                        .with_context(|| format!("GET {url}"));
                    match result {
                        Ok(bytes) => return Ok(bytes),
                        Err(err) if attempt < retries && is_retryable_http_error(&err) => {
                            let sleep_for = retry_backoff(attempt);
                            let err_summary = error_chain_summary(&err);
                            eprintln!(
                                "download failed (attempt {attempt}/{retries}) for {url}: {err_summary}; retrying in {:?}",
                                sleep_for
                            );
                            tokio::time::sleep(sleep_for).await;
                            continue;
                        }
                        Err(err) => return Err(err),
                    }
                }

                let err = anyhow!(HttpStatusFailure {
                    url: url.clone(),
                    status
                })
                .context(format!("GET {url}"));
                if attempt < retries && is_retryable_http_status(status) {
                    let sleep_for = retry_backoff(attempt);
                    eprintln!(
                        "download failed (attempt {attempt}/{retries}) for {url}: HTTP {status}; retrying in {:?}",
                        sleep_for
                    );
                    tokio::time::sleep(sleep_for).await;
                    continue;
                }
                return Err(err);
            }
            Err(err) if attempt < retries => {
                let sleep_for = retry_backoff(attempt);
                eprintln!(
                    "download failed (attempt {attempt}/{retries}) for {url}: {err}; retrying in {:?}",
                    sleep_for
                );
                tokio::time::sleep(sleep_for).await;
            }
            Err(err) => {
                return Err(err).with_context(|| format!("GET {url}"));
            }
        }
    }
}

async fn read_http_response_bytes_with_limit(
    mut resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    if let Some(encoding) = resp
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
    {
        let encoding = encoding.trim();
        if !encoding.eq_ignore_ascii_case("identity") {
            bail!("unexpected Content-Encoding: {encoding}");
        }
    }

    if let Some(len) = resp.content_length() {
        let max_u64: u64 = max_bytes.try_into().unwrap_or(u64::MAX);
        if len > max_u64 {
            bail!("response too large: max {max_bytes} bytes, got {len} (Content-Length)");
        }
    }

    let mut out = Vec::new();
    if max_bytes > 0 {
        out.reserve(max_bytes.min(1024));
    }

    while let Some(chunk) = resp.chunk().await.context("read response body chunk")? {
        if out.len().saturating_add(chunk.len()) > max_bytes {
            bail!("response too large: max {max_bytes} bytes");
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

async fn download_http_bytes_optional_with_retry(
    client: &reqwest::Client,
    url: reqwest::Url,
    retries: usize,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;

        let resp_result = client.get(url.clone()).send().await;
        match resp_result {
            Ok(resp) => {
                let status = resp.status();
                if status == reqwest::StatusCode::NOT_FOUND {
                    return Ok(None);
                }
                if status.is_success() {
                    let result = read_http_response_bytes_with_limit(resp, max_bytes)
                        .await
                        .with_context(|| format!("GET {url}"));
                    match result {
                        Ok(bytes) => return Ok(Some(bytes)),
                        Err(err) if attempt < retries && is_retryable_http_error(&err) => {
                            let sleep_for = retry_backoff(attempt);
                            let err_summary = error_chain_summary(&err);
                            eprintln!(
                                "download failed (attempt {attempt}/{retries}) for {url}: {err_summary}; retrying in {:?}",
                                sleep_for
                            );
                            tokio::time::sleep(sleep_for).await;
                            continue;
                        }
                        Err(err) => return Err(err),
                    }
                }

                let err = Err(anyhow!(HttpStatusFailure {
                    url: url.clone(),
                    status
                }))
                .with_context(|| format!("GET {url}"));

                if attempt < retries && is_retryable_http_status(status) {
                    let sleep_for = retry_backoff(attempt);
                    eprintln!(
                        "download failed (attempt {attempt}/{retries}) for {url}: HTTP {status}; retrying in {:?}",
                        sleep_for
                    );
                    tokio::time::sleep(sleep_for).await;
                    continue;
                }
                return err;
            }
            Err(err) if attempt < retries => {
                let sleep_for = retry_backoff(attempt);
                eprintln!(
                    "download failed (attempt {attempt}/{retries}) for {url}: {err}; retrying in {:?}",
                    sleep_for
                );
                tokio::time::sleep(sleep_for).await;
            }
            Err(err) => {
                return Err(err).with_context(|| format!("GET {url}"));
            }
        }
    }
}

async fn verify_http_chunk(
    index: u64,
    manifest: &ManifestV1,
    source: &VerifyHttpSource,
    retries: usize,
) -> Result<u64> {
    let expected_size = expected_chunk_size(manifest, index)?;
    let expected_sha256 = expected_chunk_sha256(manifest, index)?;

    let chunk_index_width: usize = manifest.chunk_index_width.try_into().map_err(|_| {
        anyhow!(
            "manifest chunkIndexWidth {} does not fit into usize",
            manifest.chunk_index_width
        )
    })?;
    let chunk_key = chunk_object_key_with_width(index, chunk_index_width)?;

    match source {
        VerifyHttpSource::File { base_dir } => {
            let path = base_dir.join(&chunk_key);
            verify_chunk_file(index, &path, expected_size, expected_sha256).await
        }
        VerifyHttpSource::Url {
            manifest_url,
            client,
            range_supported,
            head_supported,
        } => {
            let mut url = manifest_url.join(&chunk_key).with_context(|| {
                format!("resolve chunk url {chunk_key:?} relative to {manifest_url}")
            })?;
            // Preserve querystring auth material from the manifest URL (e.g. signed URLs).
            url.set_query(manifest_url.query());
            url.set_fragment(None);
            verify_chunk_http_with_retry(
                index,
                client,
                url,
                expected_size,
                expected_sha256,
                retries,
                Some(head_supported),
                Some(range_supported),
            )
            .await
        }
    }
}

async fn verify_chunk_file(
    index: u64,
    path: &Path,
    expected_size: u64,
    expected_sha256: Option<&str>,
) -> Result<u64> {
    // Always prefer a cheap stat to validate the expected size first.
    let meta = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("stat chunk {index} at {}", path.display()))?;
    let size = meta.len();

    if size != expected_size {
        verify_chunk_integrity(
            index,
            &format!("file://{}", path.display()),
            size,
            expected_size,
            None,
            None,
        )?;
        // `verify_chunk_integrity` should have errored, but keep a defensive guard here.
        return Ok(size);
    }

    if expected_sha256.is_none() {
        return Ok(size);
    }

    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("open chunk {index} at {}", path.display()))?;
    let mut hasher = expected_sha256.map(|_| Sha256::new());
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = file
            .read(&mut buf)
            .await
            .with_context(|| format!("read chunk {index} at {}", path.display()))?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .ok_or_else(|| anyhow!("chunk {index} size overflows u64"))?;
        if total > expected_size {
            bail!(
                "size mismatch for chunk {index} (file://{}): expected {expected_size} bytes, got at least {total} bytes",
                path.display()
            );
        }
        if let Some(hasher) = &mut hasher {
            hasher.update(&buf[..n]);
        }
    }
    verify_chunk_integrity(
        index,
        &format!("file://{}", path.display()),
        total,
        expected_size,
        hasher,
        expected_sha256,
    )?;
    Ok(total)
}

async fn verify_chunk_http(
    index: u64,
    client: &reqwest::Client,
    url: reqwest::Url,
    expected_size: u64,
    expected_sha256: Option<&str>,
    head_supported: Option<&AtomicBool>,
    range_supported: Option<&AtomicBool>,
) -> Result<u64> {
    if expected_sha256.is_none() {
        if let Some(head_supported) = head_supported {
            if head_supported.load(Ordering::SeqCst) {
                // Best-effort `HEAD` optimization: validate size via Content-Length without
                // downloading the body.
                match client.head(url.clone()).send().await {
                    Ok(resp) => {
                        let status = resp.status();
                        // Treat `HEAD` as a best-effort optimization. If it fails once for any
                        // reason, assume it is not supported (or not supported for this auth
                        // scheme) and fall back to GET for subsequent chunks.
                        if !status.is_success() {
                            head_supported.store(false, Ordering::SeqCst);
                        }

                        if status.is_success() {
                            let mut head_is_reliable = true;

                            if let Some(encoding) = resp
                                .headers()
                                .get(CONTENT_ENCODING)
                                .and_then(|v| v.to_str().ok())
                            {
                                let encoding = encoding.trim();
                                if !encoding.eq_ignore_ascii_case("identity") {
                                    // Treat HEAD as best-effort: some servers may send different
                                    // headers for HEAD than they do for GET. Fall back to GET for
                                    // definitive verification and disable future HEAD attempts.
                                    head_supported.store(false, Ordering::SeqCst);
                                    head_is_reliable = false;
                                }
                            }

                            // reqwest's `Response::content_length()` can return `None`/`0` for HEAD
                            // requests even when the server sets a `Content-Length` header.
                            // Parse the header directly so we can validate the representation size
                            // without downloading the body.
                            let len = resp
                                .headers()
                                .get(CONTENT_LENGTH)
                                .and_then(|v| v.to_str().ok())
                                .and_then(|v| v.trim().parse::<u64>().ok());
                            if head_is_reliable {
                                if let Some(len) = len {
                                    if len == expected_size {
                                        return Ok(len);
                                    }
                                    // Some servers respond to HEAD with an incorrect Content-Length
                                    // (e.g., 0) even though GET returns the correct size. Treat HEAD as
                                    // a best-effort optimization only and fall back to GET on a
                                    // mismatch to preserve correctness.
                                    head_supported.store(false, Ordering::SeqCst);
                                } else {
                                    // HEAD succeeded but didn't provide Content-Length. Avoid issuing
                                    // pointless HEAD requests for subsequent chunks.
                                    head_supported.store(false, Ordering::SeqCst);
                                }
                            }
                        }
                        // Fall back to GET if the response is not successful or doesn't provide
                        // Content-Length.
                    }
                    Err(_) => {
                        head_supported.store(false, Ordering::SeqCst);
                        // Fall back to GET on HEAD errors; GET verification will handle retries.
                    }
                }
            }
        }
    }

    let use_range = expected_sha256.is_none()
        && head_supported
            .map(|head_supported| !head_supported.load(Ordering::SeqCst))
            .unwrap_or(false)
        && range_supported
            .map(|range_supported| range_supported.load(Ordering::SeqCst))
            .unwrap_or(false);

    let mut resp = if use_range {
        client
            .get(url.clone())
            .header(RANGE, "bytes=0-0")
            .send()
            .await
            .with_context(|| format!("GET {url} (chunk {index})"))?
    } else {
        client
            .get(url.clone())
            .send()
            .await
            .with_context(|| format!("GET {url} (chunk {index})"))?
    };
    let status = resp.status();
    if use_range {
        if status == reqwest::StatusCode::PARTIAL_CONTENT {
            let Some(range_supported) = range_supported else {
                bail!("internal error: use_range true but range_supported is None");
            };

            // Best-effort range optimization: validate the total size from Content-Range without
            // downloading the full body. Disable range optimization if the response is not usable.
            let mut range_is_reliable = true;
            if let Some(encoding) = resp
                .headers()
                .get(CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
            {
                let encoding = encoding.trim();
                if !encoding.eq_ignore_ascii_case("identity") {
                    range_supported.store(false, Ordering::SeqCst);
                    range_is_reliable = false;
                }
            }

            let total = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range_total);
            if range_is_reliable {
                if let Some(total) = total {
                    if total == expected_size {
                        return Ok(expected_size);
                    }
                    // Treat range as best-effort: fall back to normal GET if the reported size
                    // does not match.
                    range_supported.store(false, Ordering::SeqCst);
                } else {
                    range_supported.store(false, Ordering::SeqCst);
                }
            }

            // Range response was not usable; fall back to a normal GET.
            resp = client
                .get(url.clone())
                .send()
                .await
                .with_context(|| format!("GET {url} (chunk {index})"))?;
        } else if status != reqwest::StatusCode::OK {
            let Some(range_supported) = range_supported else {
                bail!("internal error: use_range true but range_supported is None");
            };
            // Range request failed (or was ignored in a non-OK way). Disable and fall back.
            range_supported.store(false, Ordering::SeqCst);
            resp = client
                .get(url.clone())
                .send()
                .await
                .with_context(|| format!("GET {url} (chunk {index})"))?;
        } else {
            // Server ignored the Range header and returned 200 OK. Disable range optimization to
            // avoid sending unnecessary Range headers for subsequent chunks.
            if let Some(range_supported) = range_supported {
                range_supported.store(false, Ordering::SeqCst);
            }
        }
    }

    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!(HttpStatusFailure {
            url: url.clone(),
            status
        }))
        .with_context(|| format!("GET {url} (chunk {index})"));
    }

    if let Some(encoding) = resp
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
    {
        let encoding = encoding.trim();
        if !encoding.eq_ignore_ascii_case("identity") {
            bail!("unexpected Content-Encoding for chunk {index} ({url}): {encoding}");
        }
    }

    let content_length = resp.content_length();
    if let Some(len) = content_length {
        if len != expected_size {
            bail!(
                "size mismatch for chunk {index} ({url}): expected {expected_size} bytes, got {len} bytes (Content-Length)"
            );
        }
        // If we don't have a checksum to verify, Content-Length already validated the size. Avoid
        // downloading the body unnecessarily.
        if expected_sha256.is_none() {
            return Ok(len);
        }
    }

    let mut hasher = expected_sha256.map(|_| Sha256::new());
    let mut total: u64 = 0;
    while let Some(chunk) = resp
        .chunk()
        .await
        .with_context(|| format!("read body for GET {url} (chunk {index})"))?
    {
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| anyhow!("chunk {index} size overflows u64"))?;
        if total > expected_size {
            bail!(
                "size mismatch for chunk {index} ({url}): expected {expected_size} bytes, got at least {total} bytes"
            );
        }
        if let Some(hasher) = &mut hasher {
            hasher.update(&chunk);
        }
    }
    verify_chunk_integrity(
        index,
        url.as_str(),
        total,
        expected_size,
        hasher,
        expected_sha256,
    )?;
    Ok(total)
}

#[allow(clippy::too_many_arguments)]
async fn verify_chunk_http_with_retry(
    index: u64,
    client: &reqwest::Client,
    url: reqwest::Url,
    expected_size: u64,
    expected_sha256: Option<&str>,
    retries: usize,
    head_supported: Option<&AtomicBool>,
    range_supported: Option<&AtomicBool>,
) -> Result<u64> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        match verify_chunk_http(
            index,
            client,
            url.clone(),
            expected_size,
            expected_sha256,
            head_supported,
            range_supported,
        )
        .await
        {
            Ok(bytes) => return Ok(bytes),
            Err(err) if attempt < retries && is_retryable_http_error(&err) => {
                let sleep_for = retry_backoff(attempt);
                let err_summary = error_chain_summary(&err);
                eprintln!(
                    "chunk verify failed (attempt {attempt}/{retries}) for {url} (chunk {index}): {err_summary}; retrying in {:?}",
                    sleep_for
                );
                tokio::time::sleep(sleep_for).await;
            }
            Err(err) => {
                let root = err.root_cause().to_string();
                return Err(err).with_context(|| {
                    format!(
                        "chunk verify failed (attempt {attempt}/{retries}) for {url} (chunk {index}) (root cause: {root})"
                    )
                });
            }
        }
    }
}

fn verify_chunk_integrity(
    index: u64,
    location: &str,
    actual_size: u64,
    expected_size: u64,
    hasher: Option<Sha256>,
    expected_sha256: Option<&str>,
) -> Result<()> {
    if actual_size != expected_size {
        bail!(
            "size mismatch for chunk {index} ({location}): expected {expected_size} bytes, got {actual_size} bytes"
        );
    }
    if let Some(expected) = expected_sha256 {
        let actual = hex::encode(
            hasher
                .ok_or_else(|| anyhow!("internal error: missing hasher for chunk {index}"))?
                .finalize(),
        );
        if !actual.eq_ignore_ascii_case(expected) {
            bail!(
                "sha256 mismatch for chunk {index} ({location}): expected {expected}, got {actual}"
            );
        }
    }
    Ok(())
}

async fn verify_s3(args: &VerifyArgs) -> Result<()> {
    let bucket = args
        .bucket
        .as_deref()
        .ok_or_else(|| anyhow!("--bucket is required for S3 verification"))?;

    let s3 = build_s3_client(
        args.endpoint.as_deref(),
        args.force_path_style,
        &args.region,
    )
    .await?;

    let mut manifest_key = resolve_verify_manifest_key(args)?;
    eprintln!("Downloading s3://{}/{}...", bucket, manifest_key);

    // If the user provided `--prefix` without `--image-version`, the prefix may refer to either a
    // versioned image prefix (`.../<version>/`) or an image root (`.../<imageId>/`). If
    // `<prefix>/manifest.json` does not exist, fall back to resolving `latest.json` (if present).
    let mut latest_from_prefix: Option<(String, LatestV1)> = None;

    let manifest: ManifestV1 = match download_json_object_with_retry(
        &s3,
        bucket,
        &manifest_key,
        args.retries,
    )
    .await
    {
        Ok(manifest) => manifest,
        Err(err)
            if is_object_not_found_error(&err)
                && args.prefix.is_some()
                && args.image_version.is_none() =>
        {
            let image_root_prefix = normalize_prefix(args.prefix.as_ref().expect("prefix"));
            let latest_key = latest_object_key(&image_root_prefix);
            eprintln!(
                "Note: manifest not found at s3://{}/{}; trying s3://{}/{}...",
                bucket, manifest_key, bucket, latest_key
            );
            let latest = download_json_object_optional_with_retry::<LatestV1>(
                &s3,
                bucket,
                &latest_key,
                args.retries,
            )
            .await?
            .ok_or_else(|| {
                err.context(format!(
                    "manifest.json was not found at s3://{}/{}. If --prefix is an image root prefix, either pass --image-version, or publish a latest pointer at s3://{}/{}.",
                    bucket, manifest_key, bucket, latest_key
                ))
            })?;

            manifest_key = latest.manifest_key.clone();
            eprintln!(
                "Downloading s3://{}/{} (from latest.json)...",
                bucket, manifest_key
            );
            let manifest: ManifestV1 =
                download_json_object_with_retry(&s3, bucket, &manifest_key, args.retries).await?;
            latest_from_prefix = Some((image_root_prefix, latest));
            manifest
        }
        Err(err) => return Err(err),
    };
    validate_manifest_v1(&manifest, args.max_chunks)?;
    let manifest = Arc::new(manifest);

    if manifest.mime_type != CHUNK_MIME_TYPE {
        eprintln!(
            "Warning: manifest mimeType is '{}', expected '{}' (this does not affect integrity verification but may indicate incorrect publishing metadata).",
            manifest.mime_type, CHUNK_MIME_TYPE
        );
    }

    if let Some((image_root_prefix, latest)) = &latest_from_prefix {
        validate_latest_v1(latest, image_root_prefix, &manifest_key, manifest.as_ref())?;
    }

    let version_prefix = manifest_key
        .strip_suffix("manifest.json")
        .ok_or_else(|| anyhow!("manifest key must end with 'manifest.json', got '{manifest_key}'"))?
        .to_string();

    if let Some(expected) = &args.image_id {
        if manifest.image_id != *expected {
            bail!(
                "manifest imageId mismatch: expected '{expected}', got '{}'",
                manifest.image_id
            );
        }
    }
    if let Some(expected) = &args.image_version {
        if manifest.version != *expected {
            bail!(
                "manifest version mismatch: expected '{expected}', got '{}'",
                manifest.version
            );
        }
    }

    if args.prefix.is_some() {
        if let Some((inferred_image_id, inferred_version)) =
            infer_image_id_and_version(&version_prefix)
        {
            if manifest.image_id != inferred_image_id {
                bail!(
                    "manifest imageId '{}' does not match prefix imageId '{inferred_image_id}'",
                    manifest.image_id
                );
            }
            if manifest.version != inferred_version {
                bail!(
                    "manifest version '{}' does not match prefix version '{inferred_version}'",
                    manifest.version
                );
            }
        }
    }

    eprintln!(
        "Verifying imageId={} version={} chunkSize={} chunkCount={} totalSize={}",
        manifest.image_id,
        manifest.version,
        manifest.chunk_size,
        manifest.chunk_count,
        manifest.total_size
    );

    // Optional sanity check: if `meta.json` exists alongside the manifest, validate it matches.
    let meta_key = meta_object_key(&version_prefix);
    match download_json_object_optional_with_retry::<Meta>(&s3, bucket, &meta_key, args.retries)
        .await?
    {
        None => {
            eprintln!(
                "Note: s3://{}/{} not found; skipping meta.json validation.",
                bucket, meta_key
            );
        }
        Some(meta) => {
            validate_meta_matches_manifest(&meta, manifest.as_ref())?;
        }
    }

    // Optional sanity check: if `latest.json` exists at the inferred image root, validate it.
    // Skip if we already validated `latest.json` as part of resolving `--prefix` above.
    if latest_from_prefix.is_none() {
        if let Ok(image_root_prefix) = parent_prefix(&version_prefix) {
            let latest_key = latest_object_key(&image_root_prefix);
            match download_json_object_optional_with_retry::<LatestV1>(
                &s3,
                bucket,
                &latest_key,
                args.retries,
            )
            .await?
            {
                None => {
                    eprintln!(
                        "Note: s3://{}/{} not found; skipping latest pointer validation.",
                        bucket, latest_key
                    );
                }
                Some(latest) => {
                    validate_latest_v1(
                        &latest,
                        &image_root_prefix,
                        &manifest_key,
                        manifest.as_ref(),
                    )?;
                    // Ensure the referenced manifest exists (unless it is the one we already fetched).
                    if latest.manifest_key != manifest_key {
                        head_object_with_retry(&s3, bucket, &latest.manifest_key, args.retries)
                            .await
                            .with_context(|| {
                                format!(
                                    "latest.json points at missing manifest s3://{}/{}",
                                    bucket, latest.manifest_key
                                )
                            })?;
                    }
                }
            }
        }
    }

    verify_chunks(
        &s3,
        bucket,
        &version_prefix,
        Arc::clone(&manifest),
        args.concurrency,
        args.retries,
        args.chunk_sample,
        args.chunk_sample_seed,
    )
    .await?;

    Ok(())
}

fn validate_verify_args(args: &VerifyArgs) -> Result<()> {
    if args.concurrency == 0 {
        bail!("--concurrency must be > 0");
    }
    if args.retries == 0 {
        bail!("--retries must be > 0");
    }
    if args.max_chunks > MAX_CHUNKS {
        bail!("--max-chunks cannot exceed {MAX_CHUNKS}");
    }
    let is_http_or_file = args.manifest_url.is_some() || args.manifest_file.is_some();
    if is_http_or_file {
        if args.bucket.is_some()
            || args.prefix.is_some()
            || args.manifest_key.is_some()
            || args.endpoint.is_some()
        {
            bail!("--manifest-url/--manifest-file cannot be combined with S3 options like --bucket/--prefix/--manifest-key/--endpoint");
        }
        if !args.header.is_empty() && args.manifest_url.is_none() {
            bail!("--header can only be used with --manifest-url");
        }
    } else {
        if args.bucket.is_none() {
            bail!("either --manifest-url/--manifest-file or --bucket is required");
        }
        if args.prefix.is_none() && args.manifest_key.is_none() {
            bail!("--prefix or --manifest-key is required with --bucket");
        }
        if !args.header.is_empty() {
            bail!("--header is only valid with --manifest-url");
        }
    }
    Ok(())
}

fn resolve_verify_manifest_key(args: &VerifyArgs) -> Result<String> {
    if let Some(manifest_key) = &args.manifest_key {
        return Ok(manifest_key.clone());
    }
    let Some(prefix) = &args.prefix else {
        bail!("--prefix is required when --manifest-key is not provided");
    };
    let normalized_prefix = normalize_prefix(prefix);

    if let Some(version) = &args.image_version {
        let (version_prefix, _image_root_prefix, _resolved_image_id) =
            resolve_image_root_and_version_prefix(
                &normalized_prefix,
                args.image_id.as_deref(),
                version,
            )?;
        Ok(manifest_object_key(&version_prefix))
    } else {
        Ok(manifest_object_key(&normalized_prefix))
    }
}

fn validate_manifest_v1(manifest: &ManifestV1, max_chunks: u64) -> Result<()> {
    let sector = SECTOR_SIZE as u64;
    if manifest.schema != MANIFEST_SCHEMA {
        bail!(
            "manifest schema mismatch: expected '{MANIFEST_SCHEMA}', got '{}'",
            manifest.schema
        );
    }
    if manifest.image_id.is_empty() {
        bail!("manifest imageId must be non-empty");
    }
    if manifest.version.is_empty() {
        bail!("manifest version must be non-empty");
    }
    if manifest.mime_type.is_empty() {
        bail!("manifest mimeType must be non-empty");
    }
    if manifest.chunk_size == 0 {
        bail!("manifest chunkSize must be > 0");
    }
    if !manifest.chunk_size.is_multiple_of(sector) {
        bail!(
            "manifest chunkSize must be a multiple of {sector} bytes, got {}",
            manifest.chunk_size
        );
    }
    if manifest.chunk_size > MAX_CHUNK_SIZE_BYTES {
        bail!(
            "manifest chunkSize {} is too large (max {MAX_CHUNK_SIZE_BYTES} bytes / 64 MiB)",
            manifest.chunk_size
        );
    }
    if manifest.total_size == 0 {
        bail!("manifest totalSize must be > 0");
    }
    if !manifest.total_size.is_multiple_of(sector) {
        bail!(
            "manifest totalSize must be a multiple of {sector} bytes, got {}",
            manifest.total_size
        );
    }
    let expected_chunk_count = chunk_count(manifest.total_size, manifest.chunk_size);
    if manifest.chunk_count != expected_chunk_count {
        bail!(
            "manifest chunkCount mismatch: expected {expected_chunk_count} from totalSize/chunkSize, got {}",
            manifest.chunk_count
        );
    }
    if manifest.chunk_count > max_chunks {
        bail!(
            "manifest chunkCount {} exceeds --max-chunks {max_chunks}",
            manifest.chunk_count
        );
    }
    if manifest.chunk_index_width == 0 {
        bail!("manifest chunkIndexWidth must be > 0");
    }
    let width_usize: usize = manifest.chunk_index_width.try_into().map_err(|_| {
        anyhow!(
            "manifest chunkIndexWidth {} does not fit into usize",
            manifest.chunk_index_width
        )
    })?;
    if width_usize > 32 {
        bail!(
            "manifest chunkIndexWidth {} is unreasonably large (max 32)",
            manifest.chunk_index_width
        );
    }
    let min_width = manifest
        .chunk_count
        .saturating_sub(1)
        .to_string()
        .len()
        .max(1);
    if width_usize < min_width {
        bail!("manifest chunkIndexWidth too small: need>={min_width} got={width_usize}");
    }
    if let Some(chunks) = &manifest.chunks {
        if chunks.len() as u64 != manifest.chunk_count {
            bail!(
                "manifest chunks length {} does not match chunkCount {}",
                chunks.len(),
                manifest.chunk_count
            );
        }
        for (idx, chunk) in chunks.iter().enumerate() {
            let idx_u64: u64 = idx
                .try_into()
                .map_err(|_| anyhow!("chunk index {idx} does not fit into u64"))?;
            let expected_size =
                chunk_size_at_index(manifest.total_size, manifest.chunk_size, idx_u64)
                    .with_context(|| format!("compute expected size for chunk {idx_u64}"))?;
            let actual_size = chunk.size.unwrap_or(expected_size);
            if actual_size != expected_size {
                bail!("manifest chunk[{idx_u64}].size mismatch: expected {expected_size}, got {actual_size}");
            }
            if let Some(sha256) = &chunk.sha256 {
                validate_sha256_hex(sha256)
                    .with_context(|| format!("manifest chunk[{idx_u64}].sha256 is invalid"))?;
            }
        }
    }
    Ok(())
}

fn validate_latest_v1(
    latest: &LatestV1,
    image_root_prefix: &str,
    verified_manifest_key: &str,
    manifest: &ManifestV1,
) -> Result<()> {
    if latest.schema != LATEST_SCHEMA {
        bail!(
            "latest.json schema mismatch: expected '{LATEST_SCHEMA}', got '{}'",
            latest.schema
        );
    }
    if latest.image_id != manifest.image_id {
        bail!(
            "latest.json imageId mismatch: expected '{}', got '{}'",
            manifest.image_id,
            latest.image_id
        );
    }

    let expected_manifest_key = format!("{image_root_prefix}{}/manifest.json", latest.version);
    if latest.manifest_key != expected_manifest_key {
        bail!(
            "latest.json manifestKey mismatch: expected '{expected_manifest_key}', got '{}'",
            latest.manifest_key
        );
    }

    if latest.manifest_key == verified_manifest_key && latest.version != manifest.version {
        bail!(
            "latest.json version mismatch: manifestKey points to '{verified_manifest_key}', but latest.version is '{}' while manifest.version is '{}'",
            latest.version,
            manifest.version
        );
    }
    if latest.version == manifest.version && latest.manifest_key != verified_manifest_key {
        bail!(
            "latest.json manifestKey mismatch for version '{}': expected '{verified_manifest_key}', got '{}'",
            manifest.version,
            latest.manifest_key
        );
    }
    Ok(())
}

fn validate_meta_matches_manifest(meta: &Meta, manifest: &ManifestV1) -> Result<()> {
    if meta.total_size != manifest.total_size {
        bail!(
            "meta.json totalSize mismatch: expected {}, got {}",
            manifest.total_size,
            meta.total_size
        );
    }
    if meta.chunk_size != manifest.chunk_size {
        bail!(
            "meta.json chunkSize mismatch: expected {}, got {}",
            manifest.chunk_size,
            meta.chunk_size
        );
    }
    if meta.chunk_count != manifest.chunk_count {
        bail!(
            "meta.json chunkCount mismatch: expected {}, got {}",
            manifest.chunk_count,
            meta.chunk_count
        );
    }
    Ok(())
}

fn validate_sha256_hex(value: &str) -> Result<()> {
    if value.len() != 64 {
        bail!("expected 64 hex chars, got length {}", value.len());
    }
    if !value.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("expected hex string, got '{value}'");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn verify_chunks(
    s3: &S3Client,
    bucket: &str,
    version_prefix: &str,
    manifest: Arc<ManifestV1>,
    concurrency: usize,
    retries: usize,
    chunk_sample: Option<u64>,
    chunk_sample_seed: Option<u64>,
) -> Result<()> {
    let started_at = Instant::now();
    let chunk_count = manifest.chunk_count;

    let verify_all = match chunk_sample {
        None => true,
        // We always include the final chunk in sampling. If `N random + last` covers the entire
        // image, just verify all chunks deterministically.
        Some(n) => n.saturating_add(1) >= chunk_count,
    };

    let indices: Option<Vec<u64>> = if verify_all {
        None
    } else {
        let mut rng = match chunk_sample_seed {
            Some(seed) => fastrand::Rng::with_seed(seed),
            None => fastrand::Rng::new(),
        };
        Some(select_sampled_chunk_indices(
            chunk_count,
            chunk_sample.unwrap(),
            &mut rng,
        )?)
    };

    let total_bytes_to_verify = match &indices {
        None => manifest.total_size,
        Some(indices) => {
            let mut total: u64 = 0;
            for &idx in indices {
                total = total
                    .checked_add(expected_chunk_size(manifest.as_ref(), idx)?)
                    .ok_or_else(|| anyhow!("total bytes to verify overflows u64"))?;
            }
            total
        }
    };

    let total_chunks_to_verify = indices
        .as_ref()
        .map(|v| v.len() as u64)
        .unwrap_or(chunk_count);

    let pb = ProgressBar::new(total_bytes_to_verify);
    pb.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {bytes}/{total_bytes} {msg} ({eta})",
        )?
        .progress_chars("##-"),
    );
    pb.set_message(format!("0/{total_chunks_to_verify} chunks"));

    let chunks_checked = Arc::new(AtomicU64::new(0));
    let bytes_checked = Arc::new(AtomicU64::new(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    let chunk_index_width: usize = manifest.chunk_index_width.try_into().map_err(|_| {
        anyhow!(
            "manifest chunkIndexWidth {} does not fit into usize",
            manifest.chunk_index_width
        )
    })?;

    let work_cap = concurrency.saturating_mul(2).max(1);
    let (work_tx, work_rx) = async_channel::bounded::<u64>(work_cap);
    let (err_tx, mut err_rx) = tokio::sync::mpsc::unbounded_channel::<anyhow::Error>();

    let mut workers = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let work_rx = work_rx.clone();
        let err_tx = err_tx.clone();
        let s3 = s3.clone();
        let bucket = bucket.to_string();
        let version_prefix = version_prefix.to_string();
        let manifest = Arc::clone(&manifest);
        let pb = pb.clone();
        let chunks_checked = Arc::clone(&chunks_checked);
        let bytes_checked = Arc::clone(&bytes_checked);
        let cancelled = Arc::clone(&cancelled);
        workers.push(tokio::spawn(async move {
            while let Ok(index) = work_rx.recv().await {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }

                let result: Result<u64> = async {
                    let key = format!(
                        "{version_prefix}{}",
                        chunk_object_key_with_width(index, chunk_index_width)?
                    );
                    let expected_size = expected_chunk_size(manifest.as_ref(), index)?;
                    let expected_sha256 = expected_chunk_sha256(manifest.as_ref(), index)?;
                    verify_chunk_with_retry(
                        &s3,
                        &bucket,
                        &key,
                        expected_size,
                        expected_sha256,
                        retries,
                    )
                    .await?;
                    Ok(expected_size)
                }
                .await;

                match result {
                    Ok(bytes) => {
                        bytes_checked.fetch_add(bytes, Ordering::SeqCst);
                        pb.inc(bytes);
                        let checked = chunks_checked.fetch_add(1, Ordering::SeqCst) + 1;
                        pb.set_message(format!("{checked}/{total_chunks_to_verify} chunks"));
                    }
                    Err(err) => {
                        cancelled.store(true, Ordering::SeqCst);
                        let _ = err_tx.send(err);
                        break;
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        }));
    }
    drop(err_tx);
    // Drop the unused receiver handle so if all workers exit early (e.g. due to an internal
    // error), the producer will observe the channel closing instead of deadlocking on a full queue.
    drop(work_rx);

    let send_jobs = async {
        if let Some(indices) = indices {
            for index in indices {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                tokio::select! {
                    res = work_tx.send(index) => {
                        res.map_err(|err| anyhow!("internal worker channel closed unexpectedly: {err}"))?;
                    }
                    Some(err) = err_rx.recv() => {
                        cancelled.store(true, Ordering::SeqCst);
                        return Err(err);
                    }
                }
            }
        } else {
            for index in 0..chunk_count {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                tokio::select! {
                    res = work_tx.send(index) => {
                        res.map_err(|err| anyhow!("internal worker channel closed unexpectedly: {err}"))?;
                    }
                    Some(err) = err_rx.recv() => {
                        cancelled.store(true, Ordering::SeqCst);
                        return Err(err);
                    }
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    let send_result = send_jobs.await;
    drop(work_tx);

    if let Err(err) = send_result {
        for handle in &workers {
            handle.abort();
        }
        for handle in workers {
            let _ = handle.await;
        }
        pb.finish_and_clear();
        return Err(err);
    }

    if let Some(err) = err_rx.recv().await {
        for handle in &workers {
            handle.abort();
        }
        for handle in workers {
            let _ = handle.await;
        }
        pb.finish_and_clear();
        return Err(err);
    }

    for handle in workers {
        handle
            .await
            .map_err(|err| anyhow!("verify worker panicked: {err}"))??;
    }

    pb.finish_and_clear();

    let checked = chunks_checked.load(Ordering::SeqCst);
    let checked_bytes = bytes_checked.load(Ordering::SeqCst);
    let elapsed = started_at.elapsed();

    if checked != total_chunks_to_verify {
        bail!("internal error: only checked {checked}/{total_chunks_to_verify} chunks");
    }
    if checked_bytes != total_bytes_to_verify {
        bail!("internal error: only checked {checked_bytes}/{total_bytes_to_verify} bytes");
    }

    println!(
        "Verified {checked}/{total_chunks_to_verify} chunks ({checked_bytes} bytes) in {elapsed:.2?}"
    );
    Ok(())
}

fn select_sampled_chunk_indices(
    chunk_count: u64,
    sample: u64,
    rng: &mut fastrand::Rng,
) -> Result<Vec<u64>> {
    if chunk_count == 0 {
        return Ok(Vec::new());
    }
    let last = chunk_count - 1;
    if chunk_count == 1 {
        return Ok(vec![last]);
    }

    // Always include the final chunk, and then sample N additional chunks from `[0, last)` without
    // replacement.
    let population = last;
    let k = std::cmp::min(sample, population);
    if k == 0 {
        return Ok(vec![last]);
    }
    if k >= population {
        // All chunks were selected.
        return Ok((0..chunk_count).collect());
    }

    // Floyd's algorithm for uniform sampling without replacement in O(k) space/time (relative to
    // the sample size).
    let mut selected = BTreeSet::new();
    let start = population - k;
    for j in start..population {
        let upper = j
            .checked_add(1)
            .ok_or_else(|| anyhow!("random sampling upper bound overflows u64"))?;
        let t = rng.u64(0..upper);
        if selected.contains(&t) {
            selected.insert(j);
        } else {
            selected.insert(t);
        }
    }
    selected.insert(last);

    Ok(selected.into_iter().collect())
}

fn expected_chunk_size(manifest: &ManifestV1, index: u64) -> Result<u64> {
    if let Some(chunks) = &manifest.chunks {
        let idx: usize = index
            .try_into()
            .map_err(|_| anyhow!("chunk index {index} does not fit into usize"))?;
        let chunk = chunks
            .get(idx)
            .ok_or_else(|| anyhow!("manifest is missing entry for chunk {index}"))?;
        let derived = chunk_size_at_index(manifest.total_size, manifest.chunk_size, index)?;
        Ok(chunk.size.unwrap_or(derived))
    } else {
        chunk_size_at_index(manifest.total_size, manifest.chunk_size, index)
    }
}

fn expected_chunk_sha256(manifest: &ManifestV1, index: u64) -> Result<Option<&str>> {
    let Some(chunks) = &manifest.chunks else {
        return Ok(None);
    };
    let idx: usize = index
        .try_into()
        .map_err(|_| anyhow!("chunk index {index} does not fit into usize"))?;
    let chunk = chunks
        .get(idx)
        .ok_or_else(|| anyhow!("manifest is missing entry for chunk {index}"))?;
    Ok(chunk.sha256.as_deref())
}

async fn verify_chunk_with_retry(
    s3: &S3Client,
    bucket: &str,
    key: &str,
    expected_size: u64,
    expected_sha256: Option<&str>,
    retries: usize,
) -> Result<()> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        match verify_chunk_once(s3, bucket, key, expected_size, expected_sha256).await {
            Ok(()) => return Ok(()),
            Err(err) if attempt < retries && is_retryable_chunk_error(&err) => {
                let sleep_for = retry_backoff(attempt);
                let err_summary = error_chain_summary(&err);
                eprintln!(
                    "chunk verify failed (attempt {attempt}/{retries}) for s3://{bucket}/{key}: {err_summary}; retrying in {:?}",
                    sleep_for
                );
                tokio::time::sleep(sleep_for).await;
            }
            Err(err) => {
                let root = err.root_cause().to_string();
                return Err(err).with_context(|| {
                    format!(
                        "chunk verify failed (attempt {attempt}/{retries}) for s3://{bucket}/{key} (root cause: {root})"
                    )
                });
            }
        }
    }
}

fn is_retryable_chunk_error(err: &anyhow::Error) -> bool {
    // Treat deterministic integrity failures as non-retryable.
    //
    // Note: `anyhow::Error` `Display` only shows the top-level context by default, so inspect the
    // full error chain to reliably detect inner causes (e.g. `GET ...` wrapping `object not found
    // (404)`).
    for cause in err.chain() {
        let msg = cause.to_string();
        if msg.contains("size mismatch")
            || msg.contains("sha256 mismatch")
            || msg.contains("object not found (404)")
            || msg.contains("unexpected Content-Encoding")
            || msg.contains("access denied (403)")
        {
            return false;
        }
    }
    true
}

fn error_chain_summary(err: &anyhow::Error) -> String {
    err.chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
}

fn is_object_not_found_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("object not found (404)"))
}

async fn verify_chunk_once(
    s3: &S3Client,
    bucket: &str,
    key: &str,
    expected_size: u64,
    expected_sha256: Option<&str>,
) -> Result<()> {
    // If we don't have a checksum to validate, prefer a cheap `HEAD` request so we can validate
    // existence + Content-Length without downloading the body.
    if expected_sha256.is_none() {
        let head = s3
            .head_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|err| {
                if is_no_such_key_error(&err) {
                    anyhow!("object not found (404)")
                } else if is_access_denied_error(&err) {
                    anyhow!("access denied (403): {err}")
                } else {
                    anyhow!(err)
                }
            })
            .with_context(|| format!("HEAD s3://{bucket}/{key}"))?;

        if let Some(encoding) = head.content_encoding() {
            let encoding = encoding.trim();
            if !encoding.eq_ignore_ascii_case("identity") {
                bail!("unexpected Content-Encoding for s3://{bucket}/{key}: {encoding}");
            }
        }

        if let Some(content_length) = head.content_length() {
            let len_u64: u64 = content_length.try_into().map_err(|_| {
                anyhow!("invalid Content-Length {content_length} for s3://{bucket}/{key}")
            })?;
            if len_u64 != expected_size {
                bail!(
                    "size mismatch: expected {expected_size} bytes, got {len_u64} bytes (Content-Length)"
                );
            }
            return Ok(());
        }

        // Fall back to streaming GET if the endpoint does not supply Content-Length on HEAD.
    }

    let resp = s3
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|err| {
            if is_no_such_key_error(&err) {
                anyhow!("object not found (404)")
            } else if is_access_denied_error(&err) {
                anyhow!("access denied (403): {err}")
            } else {
                anyhow!(err)
            }
        })
        .with_context(|| format!("GET s3://{bucket}/{key}"))?;

    if let Some(encoding) = resp.content_encoding() {
        let encoding = encoding.trim();
        if !encoding.eq_ignore_ascii_case("identity") {
            bail!("unexpected Content-Encoding for s3://{bucket}/{key}: {encoding}");
        }
    }

    let content_length = resp.content_length();
    if let Some(content_length) = content_length {
        let len_u64: u64 = content_length.try_into().map_err(|_| {
            anyhow!("invalid Content-Length {content_length} for s3://{bucket}/{key}")
        })?;
        if len_u64 != expected_size {
            bail!(
                "size mismatch: expected {expected_size} bytes, got {len_u64} bytes (Content-Length)"
            );
        }

        // If we don't have a checksum to verify, Content-Length already validated the size. Avoid
        // downloading the body unnecessarily.
        if expected_sha256.is_none() {
            return Ok(());
        }
    }

    // Stream the body to validate it can be downloaded and the actual byte length matches the
    // manifest. When a sha256 checksum is provided, we hash incrementally while streaming.

    let mut reader = resp.body.into_async_read();
    let mut hasher = expected_sha256.map(|_| Sha256::new());
    let mut buf = [0u8; 64 * 1024];
    let mut read_total: u64 = 0;

    loop {
        let n = reader
            .read(&mut buf)
            .await
            .with_context(|| format!("read body of s3://{bucket}/{key}"))?;
        if n == 0 {
            break;
        }
        read_total = read_total
            .checked_add(n as u64)
            .ok_or_else(|| anyhow!("downloaded size overflows u64 for s3://{bucket}/{key}"))?;
        if read_total > expected_size {
            bail!(
                "size mismatch: expected {expected_size} bytes, got at least {read_total} bytes (streamed)"
            );
        }
        if let Some(hasher) = hasher.as_mut() {
            hasher.update(&buf[..n]);
        }
    }

    if read_total != expected_size {
        bail!("size mismatch: expected {expected_size} bytes, got {read_total} bytes (streamed)");
    }

    if let Some(expected) = expected_sha256 {
        let Some(hasher) = hasher else {
            bail!("internal error: expected_sha256 set but hasher not initialised");
        };
        let actual = hex::encode(hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            bail!("sha256 mismatch: expected {expected}, got {actual}");
        }
    }

    Ok(())
}

async fn head_object_with_retry(
    s3: &S3Client,
    bucket: &str,
    key: &str,
    retries: usize,
) -> Result<()> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let resp = s3.head_object().bucket(bucket).key(key).send().await;
        match resp {
            Ok(_) => return Ok(()),
            Err(err) if is_no_such_key_error(&err) => {
                return Err(anyhow!("object not found (404) for s3://{bucket}/{key}"));
            }
            Err(err) if is_access_denied_error(&err) => {
                return Err(anyhow!(
                    "access denied (403) for s3://{bucket}/{key}: {err}"
                ));
            }
            Err(err) if attempt < retries => {
                let sleep_for = retry_backoff(attempt);
                eprintln!(
                    "HEAD failed (attempt {attempt}/{retries}) for s3://{bucket}/{key}: {err}; retrying in {:?}",
                    sleep_for
                );
                tokio::time::sleep(sleep_for).await;
            }
            Err(err) => {
                return Err(anyhow!(
                    "HEAD failed (attempt {attempt}/{retries}) for s3://{bucket}/{key}: {err}"
                ));
            }
        }
    }
}

async fn download_json_object_with_retry<T: DeserializeOwned>(
    s3: &S3Client,
    bucket: &str,
    key: &str,
    retries: usize,
) -> Result<T> {
    let bytes = download_object_bytes_with_retry(s3, bucket, key, retries).await?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse JSON from s3://{bucket}/{key}"))
}

async fn download_json_object_optional_with_retry<T: DeserializeOwned>(
    s3: &S3Client,
    bucket: &str,
    key: &str,
    retries: usize,
) -> Result<Option<T>> {
    match download_object_bytes_optional_with_retry(s3, bucket, key, retries).await? {
        None => Ok(None),
        Some(bytes) => {
            Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("parse JSON from s3://{bucket}/{key}")
            })?))
        }
    }
}

async fn download_object_bytes_with_retry(
    s3: &S3Client,
    bucket: &str,
    key: &str,
    retries: usize,
) -> Result<Bytes> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let result = s3.get_object().bucket(bucket).key(key).send().await;
        match result {
            Ok(output) => {
                if let Some(encoding) = output.content_encoding() {
                    let encoding = encoding.trim();
                    if !encoding.eq_ignore_ascii_case("identity") {
                        bail!("unexpected Content-Encoding for s3://{bucket}/{key}: {encoding}");
                    }
                }
                if let Some(content_length) = output.content_length() {
                    let len_u64: u64 = content_length.try_into().map_err(|_| {
                        anyhow!("invalid Content-Length {content_length} for s3://{bucket}/{key}")
                    })?;
                    let max_u64: u64 = MAX_MANIFEST_JSON_BYTES.try_into().unwrap_or(u64::MAX);
                    if len_u64 > max_u64 {
                        bail!(
                            "object too large for s3://{bucket}/{key}: max {MAX_MANIFEST_JSON_BYTES} bytes, got {len_u64} (Content-Length)"
                        );
                    }
                }
                let aggregated = match output.body.collect().await {
                    Ok(aggregated) => aggregated,
                    Err(err) if attempt < retries => {
                        let sleep_for = retry_backoff(attempt);
                        eprintln!(
                            "download failed (attempt {attempt}/{retries}) for s3://{bucket}/{key}: {err}; retrying in {:?}",
                            sleep_for
                        );
                        tokio::time::sleep(sleep_for).await;
                        continue;
                    }
                    Err(err) => {
                        return Err(anyhow!(err))
                            .with_context(|| format!("read s3://{bucket}/{key}"));
                    }
                };
                let bytes = aggregated.into_bytes();
                if bytes.len() > MAX_MANIFEST_JSON_BYTES {
                    bail!(
                        "object too large for s3://{bucket}/{key}: max {MAX_MANIFEST_JSON_BYTES} bytes, got {}",
                        bytes.len()
                    );
                }
                return Ok(bytes);
            }
            Err(err) if is_no_such_key_error(&err) => {
                return Err(anyhow!("object not found (404) for s3://{bucket}/{key}"));
            }
            Err(err) if is_access_denied_error(&err) => {
                return Err(anyhow!(
                    "access denied (403) for s3://{bucket}/{key}: {err}"
                ));
            }
            Err(err) if attempt < retries => {
                let sleep_for = retry_backoff(attempt);
                eprintln!(
                    "download failed (attempt {attempt}/{retries}) for s3://{bucket}/{key}: {err}; retrying in {:?}",
                    sleep_for
                );
                tokio::time::sleep(sleep_for).await;
            }
            Err(err) => {
                return Err(anyhow!(
                    "download failed (attempt {attempt}/{retries}) for s3://{bucket}/{key}: {err}"
                ));
            }
        }
    }
}

async fn download_object_bytes_optional_with_retry(
    s3: &S3Client,
    bucket: &str,
    key: &str,
    retries: usize,
) -> Result<Option<Bytes>> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let result = s3.get_object().bucket(bucket).key(key).send().await;
        match result {
            Ok(output) => {
                if let Some(encoding) = output.content_encoding() {
                    let encoding = encoding.trim();
                    if !encoding.eq_ignore_ascii_case("identity") {
                        bail!("unexpected Content-Encoding for s3://{bucket}/{key}: {encoding}");
                    }
                }
                if let Some(content_length) = output.content_length() {
                    let len_u64: u64 = content_length.try_into().map_err(|_| {
                        anyhow!("invalid Content-Length {content_length} for s3://{bucket}/{key}")
                    })?;
                    let max_u64: u64 = MAX_MANIFEST_JSON_BYTES.try_into().unwrap_or(u64::MAX);
                    if len_u64 > max_u64 {
                        bail!(
                            "object too large for s3://{bucket}/{key}: max {MAX_MANIFEST_JSON_BYTES} bytes, got {len_u64} (Content-Length)"
                        );
                    }
                }
                let aggregated = match output.body.collect().await {
                    Ok(aggregated) => aggregated,
                    Err(err) if attempt < retries => {
                        let sleep_for = retry_backoff(attempt);
                        eprintln!(
                            "download failed (attempt {attempt}/{retries}) for s3://{bucket}/{key}: {err}; retrying in {:?}",
                            sleep_for
                        );
                        tokio::time::sleep(sleep_for).await;
                        continue;
                    }
                    Err(err) => {
                        return Err(anyhow!(err))
                            .with_context(|| format!("read s3://{bucket}/{key}"));
                    }
                };
                let bytes = aggregated.into_bytes();
                if bytes.len() > MAX_MANIFEST_JSON_BYTES {
                    bail!(
                        "object too large for s3://{bucket}/{key}: max {MAX_MANIFEST_JSON_BYTES} bytes, got {}",
                        bytes.len()
                    );
                }
                return Ok(Some(bytes));
            }
            Err(err) if is_no_such_key_error(&err) => return Ok(None),
            Err(err) if is_access_denied_error(&err) => {
                return Err(anyhow!(
                    "access denied (403) for s3://{bucket}/{key}: {err}"
                ));
            }
            Err(err) if attempt < retries => {
                let sleep_for = retry_backoff(attempt);
                eprintln!(
                    "download failed (attempt {attempt}/{retries}) for s3://{bucket}/{key}: {err}; retrying in {:?}",
                    sleep_for
                );
                tokio::time::sleep(sleep_for).await;
            }
            Err(err) => {
                return Err(anyhow!(
                    "download failed (attempt {attempt}/{retries}) for s3://{bucket}/{key}: {err}"
                ));
            }
        }
    }
}

fn is_no_such_key_error<E>(err: &aws_sdk_s3::error::SdkError<E>) -> bool
where
    E: aws_sdk_s3::error::ProvideErrorMetadata + std::fmt::Debug,
{
    // Prefer checking the HTTP status code to support S3-compatible endpoints (e.g. MinIO) where
    // the Display string may not include the canonical AWS error code.
    if matches!(sdk_error_status_code(err), Some(404)) {
        return true;
    }

    // Prefer checking for an explicit service error code (works even when Display is
    // unhelpful/empty).
    if let aws_sdk_s3::error::SdkError::ServiceError(service_err) = err {
        if matches!(
            service_err.err().meta().code(),
            Some("NoSuchKey" | "NotFound")
        ) {
            return true;
        }
    }

    // Final fallback to a best-effort string match.
    let msg = err.to_string();
    msg.contains("NoSuchKey") || msg.contains("NotFound") || msg.contains("404")
}

fn is_access_denied_error<E>(err: &aws_sdk_s3::error::SdkError<E>) -> bool
where
    E: aws_sdk_s3::error::ProvideErrorMetadata + std::fmt::Debug,
{
    if matches!(sdk_error_status_code(err), Some(403)) {
        return true;
    }

    if let aws_sdk_s3::error::SdkError::ServiceError(service_err) = err {
        if matches!(
            service_err.err().meta().code(),
            Some("AccessDenied" | "Forbidden")
        ) {
            return true;
        }
    }

    let msg = err.to_string();
    msg.contains("AccessDenied") || msg.contains("Forbidden") || msg.contains("403")
}

fn sdk_error_status_code<E>(err: &aws_sdk_s3::error::SdkError<E>) -> Option<u16> {
    use aws_sdk_s3::error::SdkError;

    match err {
        SdkError::ServiceError(service_err) => Some(service_err.raw().status().as_u16()),
        SdkError::ResponseError(resp_err) => Some(resp_err.raw().status().as_u16()),
        _ => None,
    }
}

fn validate_args(args: &PublishArgs) -> Result<()> {
    let sector = SECTOR_SIZE as u64;
    if args.chunk_size == 0 {
        bail!("--chunk-size must be > 0");
    }
    if !args.chunk_size.is_multiple_of(sector) {
        bail!("--chunk-size must be a multiple of {sector} bytes");
    }
    if args.chunk_size > MAX_CHUNK_SIZE_BYTES {
        bail!("--chunk-size too large: max {MAX_CHUNK_SIZE_BYTES} bytes (64 MiB)");
    }
    if args.concurrency == 0 {
        bail!("--concurrency must be > 0");
    }
    if args.retries == 0 {
        bail!("--retries must be > 0");
    }

    // Defence-in-depth: chunked disk streaming reads bytes by offset; intermediary transforms can
    // break deterministic byte addressing. The reference clients treat missing `no-transform` as a
    // protocol error, so reject publish configurations that would generate incompatible artifacts.
    fn has_no_transform(value: &str) -> bool {
        value
            .split(',')
            .map(|t| t.trim())
            .any(|t| t.eq_ignore_ascii_case("no-transform"))
    }

    if !has_no_transform(&args.cache_control_chunks) {
        bail!(
            "--cache-control-chunks must include 'no-transform' (got {:?})",
            args.cache_control_chunks
        );
    }
    if !has_no_transform(&args.cache_control_manifest) {
        bail!(
            "--cache-control-manifest must include 'no-transform' (got {:?})",
            args.cache_control_manifest
        );
    }
    if !has_no_transform(&args.cache_control_latest) {
        bail!(
            "--cache-control-latest must include 'no-transform' (got {:?})",
            args.cache_control_latest
        );
    }
    Ok(())
}

fn chunk_count(total_size: u64, chunk_size: u64) -> u64 {
    if total_size == 0 {
        return 0;
    }
    // Use `div_ceil` to avoid overflow when `total_size` is near `u64::MAX`.
    total_size.div_ceil(chunk_size)
}

fn normalize_prefix(prefix: &str) -> String {
    if prefix.is_empty() {
        return String::new();
    }
    if prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

#[derive(Debug)]
struct PublishDestination {
    image_id: String,
    version: String,
    /// Prefix for the versioned artifacts (must end with `/`).
    version_prefix: String,
    /// Prefix for the image root (must end with `/`), used for `latest.json`.
    image_root_prefix: String,
}

fn resolve_image_root_and_version_prefix(
    normalized_prefix: &str,
    image_id: Option<&str>,
    version: &str,
) -> Result<(String, String, String)> {
    let inferred_pair = infer_image_id_and_version(normalized_prefix);

    let segments: Vec<&str> = normalized_prefix
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    let image_id = match image_id {
        Some(image_id) => image_id.to_string(),
        None => {
            if segments.last().is_some_and(|segment| *segment == version) && segments.len() >= 2 {
                segments[segments.len() - 2].to_string()
            } else if let Some((_, inferred_version)) = inferred_pair.as_ref() {
                if looks_like_sha256_version(version)
                    && looks_like_sha256_version(inferred_version)
                    && inferred_version != version
                {
                    bail!(
                        "--prefix appears to end with sha256 version '{inferred_version}', but resolved version is '{version}'. Use a prefix ending with '/<imageId>/' (image root), or pass --image-id explicitly."
                    );
                }
                segments.last().map(|v| (*v).to_string()).ok_or_else(|| {
                    anyhow!("--image-id is required when it cannot be inferred from --prefix")
                })?
            } else {
                segments.last().map(|v| (*v).to_string()).ok_or_else(|| {
                    anyhow!("--image-id is required when it cannot be inferred from --prefix")
                })?
            }
        }
    };

    if let Some((inferred_image_id, inferred_version)) = inferred_pair.as_ref() {
        if inferred_image_id == &image_id && inferred_version != version {
            bail!(
                "--prefix appears to include version '{inferred_version}', but resolved version is '{version}'. Use a prefix ending with '/{image_id}/' or update --image-version."
            );
        }
    }

    let ends_with_version =
        segments.last().is_some_and(|segment| *segment == version) && segments.len() >= 2;
    let ends_with_image_id = segments.last().is_some_and(|segment| *segment == image_id);

    let (version_prefix, image_root_prefix) = if ends_with_version {
        let inferred_image_id = segments[segments.len() - 2];
        if inferred_image_id != image_id {
            bail!(
                "--prefix implies imageId '{inferred_image_id}', but resolved imageId is '{image_id}'. Update --prefix or --image-id."
            );
        }
        (
            normalized_prefix.to_string(),
            parent_prefix(normalized_prefix)?,
        )
    } else if ends_with_image_id {
        let image_root_prefix = normalized_prefix.to_string();
        let version_prefix = format!("{image_root_prefix}{version}/");
        (version_prefix, image_root_prefix)
    } else {
        let image_root_prefix = format!("{normalized_prefix}{image_id}/");
        let version_prefix = format!("{image_root_prefix}{version}/");
        (version_prefix, image_root_prefix)
    };

    Ok((version_prefix, image_root_prefix, image_id))
}

fn resolve_publish_destination(
    args: &PublishArgs,
    normalized_prefix: &str,
    computed_version: Option<&str>,
) -> Result<PublishDestination> {
    let inferred_pair = infer_image_id_and_version(normalized_prefix);

    let version = match computed_version {
        Some(computed) => {
            if let Some(explicit) = &args.image_version {
                if explicit != computed {
                    bail!(
                        "--image-version '{explicit}' does not match computed version '{computed}'"
                    );
                }
            }
            computed.to_string()
        }
        None => args
            .image_version
            .clone()
            .or_else(|| inferred_pair.as_ref().map(|(_, version)| version.clone()))
            .ok_or_else(|| {
                anyhow!(
                    "--image-version is required when it cannot be inferred from --prefix (or use --compute-version sha256)"
                )
            })?,
    };

    let segments: Vec<&str> = normalized_prefix
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    let image_id = match &args.image_id {
        Some(image_id) => image_id.clone(),
        None => {
            if segments.last().is_some_and(|segment| *segment == version) && segments.len() >= 2 {
                segments[segments.len() - 2].to_string()
            } else if let Some((_, inferred_version)) = inferred_pair.as_ref() {
                if looks_like_sha256_version(&version)
                    && looks_like_sha256_version(inferred_version)
                    && inferred_version != &version
                {
                    bail!(
                        "--prefix appears to end with sha256 version '{inferred_version}', but resolved version is '{version}'. Use a prefix ending with '/<imageId>/' (image root), or pass --image-id explicitly."
                    );
                }
                segments.last().map(|v| (*v).to_string()).ok_or_else(|| {
                    anyhow!("--image-id is required when it cannot be inferred from --prefix")
                })?
            } else {
                segments.last().map(|v| (*v).to_string()).ok_or_else(|| {
                    anyhow!("--image-id is required when it cannot be inferred from --prefix")
                })?
            }
        }
    };

    if let Some((inferred_image_id, inferred_version)) = inferred_pair.as_ref() {
        if inferred_image_id == &image_id && inferred_version != &version {
            bail!(
                "--prefix appears to include version '{inferred_version}', but resolved version is '{version}'. Use a prefix ending with '/{image_id}/' or update --image-version."
            );
        }
    }

    let ends_with_version =
        segments.last().is_some_and(|segment| *segment == version) && segments.len() >= 2;
    let ends_with_image_id = segments.last().is_some_and(|segment| *segment == image_id);

    let (version_prefix, image_root_prefix) = if ends_with_version {
        let inferred_image_id = segments[segments.len() - 2];
        if inferred_image_id != image_id {
            bail!(
                "--prefix implies imageId '{inferred_image_id}', but resolved imageId is '{image_id}'. Update --prefix or --image-id."
            );
        }
        (
            normalized_prefix.to_string(),
            parent_prefix(normalized_prefix)?,
        )
    } else if ends_with_image_id {
        let image_root_prefix = normalized_prefix.to_string();
        let version_prefix = format!("{image_root_prefix}{version}/");
        (version_prefix, image_root_prefix)
    } else {
        let image_root_prefix = format!("{normalized_prefix}{image_id}/");
        let version_prefix = format!("{image_root_prefix}{version}/");
        (version_prefix, image_root_prefix)
    };

    Ok(PublishDestination {
        image_id,
        version,
        version_prefix,
        image_root_prefix,
    })
}

fn parent_prefix(prefix: &str) -> Result<String> {
    let prefix = prefix.trim_end_matches('/');
    let (parent, _) = prefix
        .rsplit_once('/')
        .ok_or_else(|| anyhow!("cannot resolve parent prefix for '{prefix}'"))?;
    Ok(normalize_prefix(parent))
}

fn manifest_object_key(version_prefix: &str) -> String {
    format!("{version_prefix}manifest.json")
}

fn meta_object_key(version_prefix: &str) -> String {
    format!("{version_prefix}meta.json")
}

fn latest_object_key(image_root_prefix: &str) -> String {
    format!("{image_root_prefix}latest.json")
}

fn infer_image_id_and_version(prefix: &str) -> Option<(String, String)> {
    let segments: Vec<&str> = prefix
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    let version = segments[segments.len() - 1].to_string();
    let image_id = segments[segments.len() - 2].to_string();
    Some((image_id, version))
}

fn chunk_object_key_with_width(index: u64, width: usize) -> Result<String> {
    if index >= MAX_CHUNKS {
        bail!(
            "chunk index {index} exceeds max supported index {}",
            MAX_CHUNKS - 1
        );
    }
    if width == 0 {
        bail!("chunk index width must be > 0");
    }
    Ok(format!("chunks/{index:0width$}.bin", width = width))
}

fn chunk_object_key(index: u64) -> Result<String> {
    chunk_object_key_with_width(index, CHUNK_INDEX_WIDTH)
}

fn chunk_size_at_index(total_size: u64, chunk_size: u64, index: u64) -> Result<u64> {
    let offset = index
        .checked_mul(chunk_size)
        .ok_or_else(|| anyhow!("chunk offset overflows u64"))?;
    if offset >= total_size {
        return Ok(0);
    }
    Ok(std::cmp::min(chunk_size, total_size - offset))
}

fn build_manifest_v1(
    total_size: u64,
    chunk_size: u64,
    image_id: &str,
    version: &str,
    checksum: ChecksumAlgorithm,
    sha256_by_index: &[Option<String>],
) -> Result<ManifestV1> {
    let chunk_count = chunk_count(total_size, chunk_size);
    let mut chunks = Vec::with_capacity(chunk_count as usize);

    for index in 0..chunk_count {
        let size = chunk_size_at_index(total_size, chunk_size, index)?;

        let sha256 = match checksum {
            ChecksumAlgorithm::None => None,
            ChecksumAlgorithm::Sha256 => Some(
                sha256_by_index
                    .get(index as usize)
                    .and_then(|v| v.clone())
                    .ok_or_else(|| anyhow!("missing sha256 for chunk {index}"))?,
            ),
        };

        chunks.push(ManifestChunkV1 {
            size: Some(size),
            sha256,
        });
    }

    Ok(ManifestV1 {
        schema: MANIFEST_SCHEMA.to_string(),
        image_id: image_id.to_string(),
        version: version.to_string(),
        mime_type: CHUNK_MIME_TYPE.to_string(),
        total_size,
        chunk_size,
        chunk_count,
        chunk_index_width: CHUNK_INDEX_WIDTH as u32,
        chunks: Some(chunks),
    })
}

async fn build_s3_client(
    endpoint: Option<&str>,
    force_path_style: bool,
    region: &str,
) -> Result<S3Client> {
    let region_provider =
        RegionProviderChain::default_provider().or_else(Region::new(region.to_owned()));
    let shared_config = aws_config::defaults(BehaviorVersion::latest())
        .region(region_provider)
        .load()
        .await;

    let mut s3_config_builder = aws_sdk_s3::config::Builder::from(&shared_config);
    if let Some(endpoint) = endpoint {
        s3_config_builder = s3_config_builder.endpoint_url(endpoint);
    }
    if force_path_style {
        s3_config_builder = s3_config_builder.force_path_style(true);
    }

    Ok(S3Client::from_conf(s3_config_builder.build()))
}

#[allow(clippy::too_many_arguments)]
async fn worker_loop(
    work_rx: async_channel::Receiver<ChunkJob>,
    result_tx: tokio::sync::mpsc::Sender<ChunkResult>,
    s3: S3Client,
    bucket: String,
    prefix: String,
    cache_control_chunks: String,
    checksum: ChecksumAlgorithm,
    retries: usize,
    pb: ProgressBar,
    chunks_uploaded: Arc<AtomicU64>,
    chunk_count: u64,
) -> Result<()> {
    while let Ok(job) = work_rx.recv().await {
        let size = job.bytes.len() as u64;
        let sha256 = match checksum {
            ChecksumAlgorithm::None => None,
            ChecksumAlgorithm::Sha256 => Some(sha256_hex(job.bytes.as_ref())),
        };

        let key = format!("{}{}", prefix, chunk_object_key(job.index)?);
        put_object_with_retry(
            &s3,
            &bucket,
            &key,
            job.bytes,
            CHUNK_MIME_TYPE,
            &cache_control_chunks,
            Some(IDENTITY_CONTENT_ENCODING),
            retries,
        )
        .await?;

        pb.inc(size);
        let uploaded = chunks_uploaded.fetch_add(1, Ordering::SeqCst) + 1;
        pb.set_message(format!("{uploaded}/{chunk_count} chunks"));

        result_tx
            .send(ChunkResult {
                index: job.index,
                sha256,
            })
            .await
            .map_err(|err| anyhow!("internal result channel closed unexpectedly: {err}"))?;
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChunkCheckError {
    SizeMismatch { expected: u64, actual: u64 },
    Sha256Mismatch { expected: String, actual: String },
}

impl std::fmt::Display for ChunkCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SizeMismatch { expected, actual } => {
                write!(
                    f,
                    "size mismatch: expected {expected} bytes, got {actual} bytes"
                )
            }
            Self::Sha256Mismatch { expected, actual } => {
                write!(f, "sha256 mismatch: expected {expected}, got {actual}")
            }
        }
    }
}

#[allow(dead_code)]
fn check_chunk_bytes(
    bytes: &[u8],
    expected_size: u64,
    expected_sha256: Option<&str>,
) -> std::result::Result<(), ChunkCheckError> {
    let actual_size = bytes.len() as u64;
    if actual_size != expected_size {
        return Err(ChunkCheckError::SizeMismatch {
            expected: expected_size,
            actual: actual_size,
        });
    }
    if let Some(expected_sha256) = expected_sha256 {
        let actual_sha256 = sha256_hex(bytes);
        if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
            return Err(ChunkCheckError::Sha256Mismatch {
                expected: expected_sha256.to_string(),
                actual: actual_sha256,
            });
        }
    }
    Ok(())
}

fn sha256_version_from_digest(digest: impl AsRef<[u8]>) -> String {
    format!("sha256-{}", hex::encode(digest))
}

fn looks_like_sha256_version(version: &str) -> bool {
    let Some(hex_digest) = version.strip_prefix("sha256-") else {
        return false;
    };
    hex_digest.len() == 64 && hex_digest.chars().all(|c| c.is_ascii_hexdigit())
}

fn open_input_disk(path: &Path, format: InputFormat) -> Result<DiskImage<FileBackend>> {
    let backend =
        FileBackend::open_read_only(path).with_context(|| format!("open {}", path.display()))?;

    let disk =
        match format {
            InputFormat::Raw => DiskImage::open_with_format(DiskFormat::Raw, backend)
                .context("open raw disk image")?,
            InputFormat::AeroSparse => DiskImage::open_with_format(DiskFormat::AeroSparse, backend)
                .context("open aerosparse disk image")?,
            InputFormat::Qcow2 => DiskImage::open_with_format(DiskFormat::Qcow2, backend)
                .context("open qcow2 disk image")?,
            InputFormat::Vhd => DiskImage::open_with_format(DiskFormat::Vhd, backend)
                .context("open vhd disk image")?,
            InputFormat::Auto => DiskImage::open_auto(backend).context("open disk image (auto)")?,
        };

    Ok(disk)
}

fn inspect_input_disk(path: &Path, format: InputFormat) -> Result<(DiskFormat, u64)> {
    let disk = open_input_disk(path, format)?;
    Ok((disk.format(), disk.capacity_bytes()))
}

async fn compute_image_version_sha256(path: &Path, format: InputFormat) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut disk = open_input_disk(&path, format)?;
        let total_size = disk.capacity_bytes();
        if total_size == 0 {
            bail!("virtual disk size must be > 0");
        }

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        let mut offset = 0u64;

        while offset < total_size {
            let remaining = total_size - offset;
            let len = (buf.len() as u64).min(remaining) as usize;
            disk.read_at(offset, &mut buf[..len]).with_context(|| {
                format!(
                    "read {} while hashing at offset={} len={}",
                    path.display(),
                    offset,
                    len
                )
            })?;
            hasher.update(&buf[..len]);
            offset = offset
                .checked_add(len as u64)
                .ok_or_else(|| anyhow!("hash offset overflows u64"))?;
        }

        Ok::<_, anyhow::Error>(sha256_version_from_digest(hasher.finalize()))
    })
    .await
    .map_err(|err| anyhow!("hash worker panicked: {err}"))?
}

async fn upload_json_object<T: Serialize>(
    s3: &S3Client,
    bucket: &str,
    key: &str,
    value: &T,
    cache_control: &str,
    retries: usize,
) -> Result<()> {
    let json = serde_json::to_vec_pretty(value).context("serialize json")?;
    put_object_with_retry(
        s3,
        bucket,
        key,
        Bytes::from(json),
        JSON_MIME_TYPE,
        cache_control,
        Some(IDENTITY_CONTENT_ENCODING),
        retries,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn put_object_with_retry(
    s3: &S3Client,
    bucket: &str,
    key: &str,
    body: Bytes,
    content_type: &str,
    cache_control: &str,
    content_encoding: Option<&str>,
    retries: usize,
) -> Result<()> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let mut request = s3
            .put_object()
            .bucket(bucket)
            .key(key)
            .content_type(content_type)
            .cache_control(cache_control)
            .body(ByteStream::from(body.clone()));
        if let Some(content_encoding) = content_encoding {
            request = request.content_encoding(content_encoding);
        }
        let result = request.send().await;
        match result {
            Ok(_) => return Ok(()),
            Err(err) if attempt < retries => {
                let sleep_for = retry_backoff(attempt);
                eprintln!(
                    "upload failed (attempt {attempt}/{retries}) for s3://{bucket}/{key}: {err}; retrying in {:?}",
                    sleep_for
                );
                tokio::time::sleep(sleep_for).await;
            }
            Err(err) => {
                return Err(anyhow!(
                    "upload failed (attempt {attempt}/{retries}) for s3://{bucket}/{key}: {err}"
                ));
            }
        }
    }
}

fn retry_backoff(attempt: usize) -> Duration {
    let exp = attempt.saturating_sub(1).min(10) as u32;
    let base_ms = 200u64.saturating_mul(2u64.saturating_pow(exp));
    let jitter_ms = fastrand::u64(0..200);
    Duration::from_millis(base_ms.saturating_add(jitter_ms).min(10_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct TestHttpRequest {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
    }

    async fn start_test_http_server(
        responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        >,
    ) -> Result<(
        String,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    )> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind test http listener")?;
        let addr = listener
            .local_addr()
            .context("get test http listener address")?;
        let base_url = format!("http://{}", addr);

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let (mut socket, _) = match accept {
                            Ok(v) => v,
                            Err(_) => break,
                        };
                        let responder = Arc::clone(&responder);
                        tokio::spawn(async move {
                            // Read request headers (best-effort).
                            let mut buf = Vec::new();
                            let mut scratch = [0u8; 1024];
                            while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8 * 1024 {
                                let n = match socket.read(&mut scratch).await {
                                    Ok(0) => break,
                                    Ok(n) => n,
                                    Err(_) => return,
                                };
                                buf.extend_from_slice(&scratch[..n]);
                            }

                            let raw = String::from_utf8_lossy(&buf).to_string();
                            let (method, path) = raw
                                .lines()
                                .next()
                                .and_then(|line| {
                                    let mut parts = line.split_whitespace();
                                    Some((
                                        parts.next()?.to_string(),
                                        parts.next()?.to_string(),
                                    ))
                                })
                                .unwrap_or_else(|| ("GET".to_string(), "/".to_string()));
                            let is_head = method.eq_ignore_ascii_case("HEAD");
                            let mut headers = Vec::new();
                            for line in raw.lines().skip(1) {
                                let line = line.trim();
                                if line.is_empty() {
                                    break;
                                }
                                if let Some((name, value)) = line.split_once(':') {
                                    headers.push((name.trim().to_string(), value.trim().to_string()));
                                }
                            }

                            let (status, extra_headers, body) =
                                (responder)(TestHttpRequest { method, path, headers });
                            let reason = match status {
                                200 => "OK",
                                400 => "Bad Request",
                                401 => "Unauthorized",
                                403 => "Forbidden",
                                404 => "Not Found",
                                405 => "Method Not Allowed",
                                408 => "Request Timeout",
                                429 => "Too Many Requests",
                                500 => "Internal Server Error",
                                501 => "Not Implemented",
                                _ => "Unknown",
                            };
                            let content_length = extra_headers
                                .iter()
                                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .map(|(_, v)| v.trim().to_string())
                                .unwrap_or_else(|| body.len().to_string());
                            let mut headers = format!(
                                "HTTP/1.1 {status} {reason}\r\nContent-Length: {content_length}\r\n"
                            );
                            for (name, value) in extra_headers {
                                if name.eq_ignore_ascii_case("content-length")
                                    || name.eq_ignore_ascii_case("connection")
                                {
                                    continue;
                                }
                                headers.push_str(name.trim());
                                headers.push_str(": ");
                                headers.push_str(value.trim());
                                headers.push_str("\r\n");
                            }
                            headers.push_str("Connection: close\r\n\r\n");

                            let _ = socket.write_all(headers.as_bytes()).await;
                            // Match real HTTP semantics: HEAD responses should not include a body,
                            // but typically still include a Content-Length describing the
                            // corresponding GET representation size.
                            if !is_head {
                                let _ = socket.write_all(&body).await;
                            }
                            let _ = socket.shutdown().await;
                        });
                    }
                }
            }
        });

        Ok((base_url, shutdown_tx, handle))
    }

    #[tokio::test]
    async fn download_http_bytes_with_retry_rejects_oversized_response() -> Result<()> {
        let (base_url, shutdown_tx, handle) = start_test_http_server(Arc::new(|_req| {
            let body = vec![b'a'; 11];
            (
                200,
                vec![("Content-Type".to_string(), "application/json".to_string())],
                body,
            )
        }))
        .await?;

        let url: reqwest::Url = format!("{base_url}/manifest.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let err = download_http_bytes_with_retry(&client, url, 1, 10)
            .await
            .expect_err("expected download to be rejected");
        assert!(
            err.root_cause()
                .to_string()
                .to_ascii_lowercase()
                .contains("too large"),
            "unexpected error chain: {}",
            error_chain_summary(&err)
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_with_retry_does_not_retry_on_oversized_response() -> Result<()> {
        let requests = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let requests = Arc::clone(&requests);
            Arc::new(move |_req: TestHttpRequest| {
                requests.fetch_add(1, Ordering::SeqCst);
                (200, Vec::new(), vec![b'a'; 11])
            })
        };
        let (base_url, shutdown_tx, handle) = start_test_http_server(responder).await?;

        let url: reqwest::Url = format!("{base_url}/manifest.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let err = download_http_bytes_with_retry(&client, url, 3, 10)
            .await
            .expect_err("expected oversized download to be rejected");
        assert!(
            error_chain_summary(&err)
                .to_ascii_lowercase()
                .contains("too large"),
            "unexpected error chain: {}",
            error_chain_summary(&err)
        );
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "expected oversized response to be treated as non-retryable"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_with_retry_retries_on_429() -> Result<()> {
        let requests = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let requests = Arc::clone(&requests);
            Arc::new(move |_req: TestHttpRequest| {
                let n = requests.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    (429, Vec::new(), b"too many".to_vec())
                } else {
                    (200, Vec::new(), b"ok".to_vec())
                }
            })
        };
        let (base_url, shutdown_tx, handle) = start_test_http_server(responder).await?;

        let url: reqwest::Url = format!("{base_url}/manifest.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let bytes = download_http_bytes_with_retry(&client, url, 2, 1024)
            .await
            .context("download with retry")?;
        assert_eq!(bytes.as_slice(), b"ok");
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "expected at least one retry after HTTP 429"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_with_retry_retries_on_408() -> Result<()> {
        let requests = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let requests = Arc::clone(&requests);
            Arc::new(move |_req: TestHttpRequest| {
                let n = requests.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    (408, Vec::new(), b"timeout".to_vec())
                } else {
                    (200, Vec::new(), b"ok".to_vec())
                }
            })
        };
        let (base_url, shutdown_tx, handle) = start_test_http_server(responder).await?;

        let url: reqwest::Url = format!("{base_url}/manifest.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let bytes = download_http_bytes_with_retry(&client, url, 2, 1024)
            .await
            .context("download with retry")?;
        assert_eq!(bytes.as_slice(), b"ok");
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "expected at least one retry after HTTP 408"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_with_retry_does_not_retry_on_404() -> Result<()> {
        let requests = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let requests = Arc::clone(&requests);
            Arc::new(move |_req: TestHttpRequest| {
                requests.fetch_add(1, Ordering::SeqCst);
                (404, Vec::new(), b"not found".to_vec())
            })
        };
        let (base_url, shutdown_tx, handle) = start_test_http_server(responder).await?;

        let url: reqwest::Url = format!("{base_url}/manifest.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let err = download_http_bytes_with_retry(&client, url, 3, 1024)
            .await
            .expect_err("expected 404 to be treated as a hard failure");
        assert!(
            error_chain_summary(&err).contains("HTTP 404"),
            "unexpected error chain: {}",
            error_chain_summary(&err)
        );
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "expected downloader to not retry HTTP 404"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_with_retry_retries_on_transient_500() -> Result<()> {
        let requests = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let requests = Arc::clone(&requests);
            Arc::new(move |_req: TestHttpRequest| {
                let n = requests.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    (500, Vec::new(), b"oops".to_vec())
                } else {
                    (200, Vec::new(), b"ok".to_vec())
                }
            })
        };
        let (base_url, shutdown_tx, handle) = start_test_http_server(responder).await?;

        let url: reqwest::Url = format!("{base_url}/manifest.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let bytes = download_http_bytes_with_retry(&client, url, 2, 1024)
            .await
            .context("download with retry")?;
        assert_eq!(bytes.as_slice(), b"ok");
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "expected at least one retry after HTTP 500"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_with_retry_retries_on_truncated_body() -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind test listener")?;
        let addr = listener.local_addr().context("get listener addr")?;

        let requests = Arc::new(AtomicU64::new(0));
        let requests_for_server = Arc::clone(&requests);

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let (mut socket, _) = accept?;
                        let mut buf = [0u8; 1024];
                        let _ = socket.read(&mut buf).await?;

                        let n = requests_for_server.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            // Declare Content-Length=2 but only send 1 byte, causing the client to
                            // error while reading the body.
                            socket
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\no")
                                .await?;
                        } else {
                            socket
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                                .await?;
                        }
                        socket.shutdown().await?;
                        if n >= 1 {
                            break;
                        }
                    }
                }
            }
            Ok::<(), std::io::Error>(())
        });

        let url: reqwest::Url = format!("http://{addr}/manifest.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let bytes = download_http_bytes_with_retry(&client, url, 2, 1024)
            .await
            .context("download with retry")?;
        assert_eq!(bytes.as_slice(), b"ok");
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "expected at least one retry after truncated body"
        );

        let _ = shutdown_tx.send(());
        handle
            .await
            .map_err(|err| anyhow!("test server panicked: {err}"))??;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_with_retry_rejects_unexpected_content_encoding() -> Result<()> {
        let (base_url, shutdown_tx, handle) = start_test_http_server(Arc::new(|_req| {
            (
                200,
                vec![("Content-Encoding".to_string(), "gzip".to_string())],
                b"ok".to_vec(),
            )
        }))
        .await?;

        let url: reqwest::Url = format!("{base_url}/manifest.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let err = download_http_bytes_with_retry(&client, url, 1, 1024)
            .await
            .expect_err("expected unexpected Content-Encoding to be rejected");
        assert!(
            error_chain_summary(&err).contains("unexpected Content-Encoding"),
            "unexpected error chain: {}",
            error_chain_summary(&err)
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_with_retry_rejects_oversized_chunked_response_without_content_length(
    ) -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind test listener")?;
        let addr = listener.local_addr().context("get listener addr")?;
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await?;

            let body = vec![b'a'; 11];
            let header =
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
            socket.write_all(header).await?;
            socket
                .write_all(format!("{:x}\r\n", body.len()).as_bytes())
                .await?;
            socket.write_all(&body).await?;
            socket.write_all(b"\r\n0\r\n\r\n").await?;
            socket.shutdown().await?;
            Ok::<(), std::io::Error>(())
        });

        let url: reqwest::Url = format!("http://{addr}/manifest.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let err = download_http_bytes_with_retry(&client, url, 1, 10)
            .await
            .expect_err("expected download to be rejected");
        assert!(
            err.root_cause()
                .to_string()
                .to_ascii_lowercase()
                .contains("too large"),
            "unexpected error chain: {}",
            error_chain_summary(&err)
        );

        handle
            .await
            .map_err(|err| anyhow!("test server panicked: {err}"))??;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_optional_with_retry_returns_none_on_404() -> Result<()> {
        let requests = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let requests = Arc::clone(&requests);
            Arc::new(move |_req: TestHttpRequest| {
                requests.fetch_add(1, Ordering::SeqCst);
                (404, Vec::new(), b"not found".to_vec())
            })
        };

        let (base_url, shutdown_tx, handle) = start_test_http_server(responder).await?;

        let url: reqwest::Url = format!("{base_url}/meta.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let bytes = download_http_bytes_optional_with_retry(&client, url, 3, 1024)
            .await
            .context("download optional")?;
        assert!(bytes.is_none(), "expected None on 404");
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "expected the optional downloader to not retry 404"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_optional_with_retry_does_not_retry_on_oversized_response(
    ) -> Result<()> {
        let requests = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let requests = Arc::clone(&requests);
            Arc::new(move |_req: TestHttpRequest| {
                requests.fetch_add(1, Ordering::SeqCst);
                (200, Vec::new(), vec![b'a'; 11])
            })
        };

        let (base_url, shutdown_tx, handle) = start_test_http_server(responder).await?;

        let url: reqwest::Url = format!("{base_url}/meta.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let err = download_http_bytes_optional_with_retry(&client, url, 3, 10)
            .await
            .expect_err("expected oversized download to be rejected");
        assert!(
            error_chain_summary(&err)
                .to_ascii_lowercase()
                .contains("too large"),
            "unexpected error chain: {}",
            error_chain_summary(&err)
        );
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "expected oversized response to be treated as non-retryable"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_optional_with_retry_retries_on_transient_500() -> Result<()> {
        let requests = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let requests = Arc::clone(&requests);
            Arc::new(move |_req: TestHttpRequest| {
                let n = requests.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    (500, Vec::new(), b"oops".to_vec())
                } else {
                    (200, Vec::new(), b"ok".to_vec())
                }
            })
        };

        let (base_url, shutdown_tx, handle) = start_test_http_server(responder).await?;

        let url: reqwest::Url = format!("{base_url}/meta.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let bytes = download_http_bytes_optional_with_retry(&client, url, 2, 1024)
            .await
            .context("download optional")?;
        assert_eq!(bytes.as_deref(), Some(b"ok".as_ref()));
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "expected at least one retry after HTTP 500"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_optional_with_retry_retries_on_429() -> Result<()> {
        let requests = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let requests = Arc::clone(&requests);
            Arc::new(move |_req: TestHttpRequest| {
                let n = requests.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    (429, Vec::new(), b"too many".to_vec())
                } else {
                    (200, Vec::new(), b"ok".to_vec())
                }
            })
        };

        let (base_url, shutdown_tx, handle) = start_test_http_server(responder).await?;

        let url: reqwest::Url = format!("{base_url}/meta.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let bytes = download_http_bytes_optional_with_retry(&client, url, 2, 1024)
            .await
            .context("download optional")?;
        assert_eq!(bytes.as_deref(), Some(b"ok".as_ref()));
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "expected at least one retry after HTTP 429"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_optional_with_retry_retries_on_408() -> Result<()> {
        let requests = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let requests = Arc::clone(&requests);
            Arc::new(move |_req: TestHttpRequest| {
                let n = requests.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    (408, Vec::new(), b"timeout".to_vec())
                } else {
                    (200, Vec::new(), b"ok".to_vec())
                }
            })
        };

        let (base_url, shutdown_tx, handle) = start_test_http_server(responder).await?;

        let url: reqwest::Url = format!("{base_url}/meta.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let bytes = download_http_bytes_optional_with_retry(&client, url, 2, 1024)
            .await
            .context("download optional")?;
        assert_eq!(bytes.as_deref(), Some(b"ok".as_ref()));
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "expected at least one retry after HTTP 408"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn download_http_bytes_optional_with_retry_retries_on_truncated_body() -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind test listener")?;
        let addr = listener.local_addr().context("get listener addr")?;

        let requests = Arc::new(AtomicU64::new(0));
        let requests_for_server = Arc::clone(&requests);

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let (mut socket, _) = accept?;
                        let mut buf = [0u8; 1024];
                        let _ = socket.read(&mut buf).await?;

                        let n = requests_for_server.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            socket
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\no")
                                .await?;
                        } else {
                            socket
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                                .await?;
                        }
                        socket.shutdown().await?;
                        if n >= 1 {
                            break;
                        }
                    }
                }
            }
            Ok::<(), std::io::Error>(())
        });

        let url: reqwest::Url = format!("http://{addr}/meta.json").parse().unwrap();
        let client = build_reqwest_client(&[])?;
        let bytes = download_http_bytes_optional_with_retry(&client, url, 2, 1024)
            .await
            .context("download optional with retry")?;
        assert_eq!(bytes.as_deref(), Some(b"ok".as_ref()));
        assert!(
            requests.load(Ordering::SeqCst) >= 2,
            "expected at least one retry after truncated body"
        );

        let _ = shutdown_tx.send(());
        handle
            .await
            .map_err(|err| anyhow!("test server panicked: {err}"))??;
        Ok(())
    }

    #[test]
    fn default_cache_control_values_match_docs() {
        assert_eq!(
            DEFAULT_CACHE_CONTROL_CHUNKS,
            "public, max-age=31536000, immutable, no-transform"
        );
        assert_eq!(
            DEFAULT_CACHE_CONTROL_MANIFEST,
            "public, max-age=31536000, immutable, no-transform"
        );
        assert_eq!(
            DEFAULT_CACHE_CONTROL_LATEST,
            "public, max-age=60, no-transform"
        );
    }

    #[test]
    fn default_chunk_size_is_4_mib() {
        assert_eq!(DEFAULT_CHUNK_SIZE_BYTES, 4 * 1024 * 1024);
    }

    #[test]
    fn validate_args_rejects_cache_control_without_no_transform() {
        let mut args = PublishArgs {
            file: PathBuf::from("disk.img"),
            format: InputFormat::Raw,
            bucket: "bucket".to_string(),
            prefix: "images/win7/v1/".to_string(),
            image_id: None,
            image_version: Some("v1".to_string()),
            compute_version: ComputeVersion::None,
            publish_latest: false,
            cache_control_chunks: DEFAULT_CACHE_CONTROL_CHUNKS.to_string(),
            cache_control_manifest: DEFAULT_CACHE_CONTROL_MANIFEST.to_string(),
            cache_control_latest: DEFAULT_CACHE_CONTROL_LATEST.to_string(),
            chunk_size: DEFAULT_CHUNK_SIZE_BYTES,
            checksum: ChecksumAlgorithm::Sha256,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: DEFAULT_CONCURRENCY,
            retries: DEFAULT_RETRIES,
            no_meta: false,
        };

        args.cache_control_chunks = "public, max-age=60".to_string();
        let err = validate_args(&args).expect_err("expected cache_control_chunks failure");
        assert!(
            err.to_string().contains("--cache-control-chunks"),
            "unexpected error: {err}"
        );

        args.cache_control_chunks = DEFAULT_CACHE_CONTROL_CHUNKS.to_string();
        args.cache_control_manifest = "public, max-age=60".to_string();
        let err = validate_args(&args).expect_err("expected cache_control_manifest failure");
        assert!(
            err.to_string().contains("--cache-control-manifest"),
            "unexpected error: {err}"
        );

        args.cache_control_manifest = DEFAULT_CACHE_CONTROL_MANIFEST.to_string();
        args.cache_control_latest = "public, max-age=60".to_string();
        let err = validate_args(&args).expect_err("expected cache_control_latest failure");
        assert!(
            err.to_string().contains("--cache-control-latest"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn prefix_normalization_adds_trailing_slash() {
        assert_eq!(normalize_prefix("images/foo"), "images/foo/");
        assert_eq!(normalize_prefix("images/foo/"), "images/foo/");
        assert_eq!(normalize_prefix(""), "");
    }

    #[test]
    fn chunk_key_is_fixed_width() -> Result<()> {
        assert_eq!(chunk_object_key(0)?, "chunks/00000000.bin");
        assert_eq!(chunk_object_key(1)?, "chunks/00000001.bin");
        assert_eq!(chunk_object_key(42)?, "chunks/00000042.bin");
        Ok(())
    }

    #[test]
    fn chunk_object_key_with_width_formats_with_padding() -> Result<()> {
        assert_eq!(chunk_object_key_with_width(0, 1)?, "chunks/0.bin");
        assert_eq!(chunk_object_key_with_width(0, 4)?, "chunks/0000.bin");
        assert_eq!(chunk_object_key_with_width(42, 4)?, "chunks/0042.bin");
        Ok(())
    }

    #[test]
    fn chunk_object_key_with_width_rejects_zero_width() {
        let err = chunk_object_key_with_width(0, 0).expect_err("expected validation failure");
        assert!(
            err.to_string().contains("width must be > 0"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn chunk_object_key_with_width_rejects_index_out_of_range() {
        let err = chunk_object_key_with_width(MAX_CHUNKS, 8).expect_err("expected failure");
        assert!(
            err.to_string().contains("exceeds max supported index"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn chunk_size_at_index_returns_zero_when_offset_is_past_end() -> Result<()> {
        assert_eq!(chunk_size_at_index(8, 4, 2)?, 0);
        Ok(())
    }

    #[test]
    fn chunk_size_at_index_rejects_offset_overflow() {
        let err = chunk_size_at_index(16, u64::MAX, 2).expect_err("expected overflow error");
        assert!(
            err.to_string().contains("offset overflows"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn infer_image_id_and_version_from_prefix() {
        assert_eq!(
            infer_image_id_and_version("images/win7/sha256-abc/"),
            Some(("win7".to_string(), "sha256-abc".to_string()))
        );
        assert_eq!(
            infer_image_id_and_version("win7/sha256-abc"),
            Some(("win7".to_string(), "sha256-abc".to_string()))
        );
        assert_eq!(infer_image_id_and_version(""), None);
        assert_eq!(infer_image_id_and_version("onlyone/"), None);
    }

    #[test]
    fn resolve_image_root_and_version_prefix_accepts_versioned_prefix() -> Result<()> {
        let normalized_prefix = normalize_prefix("images/win7/sha256-abc/");
        let (version_prefix, image_root_prefix, image_id) =
            resolve_image_root_and_version_prefix(&normalized_prefix, None, "sha256-abc")?;
        assert_eq!(image_id, "win7");
        assert_eq!(image_root_prefix, "images/win7/");
        assert_eq!(version_prefix, "images/win7/sha256-abc/");
        Ok(())
    }

    #[test]
    fn resolve_image_root_and_version_prefix_appends_to_image_root_prefix() -> Result<()> {
        let normalized_prefix = normalize_prefix("images/win7/");
        let (version_prefix, image_root_prefix, image_id) =
            resolve_image_root_and_version_prefix(&normalized_prefix, None, "sha256-abc")?;
        assert_eq!(image_id, "win7");
        assert_eq!(image_root_prefix, "images/win7/");
        assert_eq!(version_prefix, "images/win7/sha256-abc/");
        Ok(())
    }

    #[test]
    fn resolve_image_root_and_version_prefix_adds_image_id_when_prefix_is_parent() -> Result<()> {
        let normalized_prefix = normalize_prefix("images/");
        let (version_prefix, image_root_prefix, image_id) =
            resolve_image_root_and_version_prefix(&normalized_prefix, Some("win7"), "sha256-abc")?;
        assert_eq!(image_id, "win7");
        assert_eq!(image_root_prefix, "images/win7/");
        assert_eq!(version_prefix, "images/win7/sha256-abc/");
        Ok(())
    }

    #[test]
    fn resolve_image_root_and_version_prefix_rejects_sha256_version_mismatch() {
        let inferred_version = sha256_version_from_digest([0u8; 32]);
        let resolved_version = sha256_version_from_digest([1u8; 32]);

        let normalized_prefix = normalize_prefix(&format!("images/win7/{inferred_version}/"));
        let err =
            resolve_image_root_and_version_prefix(&normalized_prefix, None, &resolved_version)
                .expect_err("expected version mismatch error");
        let msg = err.to_string();
        assert!(
            msg.contains("prefix appears to end with sha256 version")
                && msg.contains(&inferred_version)
                && msg.contains(&resolved_version),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn resolve_image_root_and_version_prefix_rejects_versioned_prefix_mismatch_with_explicit_image_id(
    ) {
        let normalized_prefix = normalize_prefix("images/win7/v1/");
        let err = resolve_image_root_and_version_prefix(&normalized_prefix, Some("win7"), "v2")
            .expect_err("expected version mismatch error");
        let msg = err.to_string();
        assert!(
            msg.contains("appears to include version") && msg.contains("v1") && msg.contains("v2"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn resolve_image_root_and_version_prefix_rejects_implied_image_id_mismatch() {
        let normalized_prefix = normalize_prefix("images/win7/v1/");
        let err = resolve_image_root_and_version_prefix(&normalized_prefix, Some("other"), "v1")
            .expect_err("expected imageId mismatch error");
        let msg = err.to_string();
        assert!(
            msg.contains("prefix implies imageId") && msg.contains("win7") && msg.contains("other"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn resolve_publish_destination_infers_from_versioned_prefix() -> Result<()> {
        let args = PublishArgs {
            file: PathBuf::from("disk.img"),
            format: InputFormat::Raw,
            bucket: "bucket".to_string(),
            prefix: "images/win7/sha256-abc/".to_string(),
            image_id: None,
            image_version: None,
            compute_version: ComputeVersion::None,
            publish_latest: false,
            cache_control_chunks: DEFAULT_CACHE_CONTROL_CHUNKS.to_string(),
            cache_control_manifest: DEFAULT_CACHE_CONTROL_MANIFEST.to_string(),
            cache_control_latest: DEFAULT_CACHE_CONTROL_LATEST.to_string(),
            chunk_size: DEFAULT_CHUNK_SIZE_BYTES,
            checksum: ChecksumAlgorithm::Sha256,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: DEFAULT_CONCURRENCY,
            retries: DEFAULT_RETRIES,
            no_meta: false,
        };
        let prefix = normalize_prefix(&args.prefix);
        let dest = resolve_publish_destination(&args, &prefix, None)?;
        assert_eq!(dest.image_id, "win7");
        assert_eq!(dest.version, "sha256-abc");
        assert_eq!(dest.version_prefix, "images/win7/sha256-abc/");
        assert_eq!(dest.image_root_prefix, "images/win7/");
        Ok(())
    }

    #[test]
    fn resolve_publish_destination_appends_computed_version_to_image_root() -> Result<()> {
        let args = PublishArgs {
            file: PathBuf::from("disk.img"),
            format: InputFormat::Raw,
            bucket: "bucket".to_string(),
            prefix: "images/win7/".to_string(),
            image_id: None,
            image_version: None,
            compute_version: ComputeVersion::None,
            publish_latest: false,
            cache_control_chunks: DEFAULT_CACHE_CONTROL_CHUNKS.to_string(),
            cache_control_manifest: DEFAULT_CACHE_CONTROL_MANIFEST.to_string(),
            cache_control_latest: DEFAULT_CACHE_CONTROL_LATEST.to_string(),
            chunk_size: DEFAULT_CHUNK_SIZE_BYTES,
            checksum: ChecksumAlgorithm::Sha256,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: DEFAULT_CONCURRENCY,
            retries: DEFAULT_RETRIES,
            no_meta: false,
        };
        let prefix = normalize_prefix(&args.prefix);
        let dest = resolve_publish_destination(&args, &prefix, Some("sha256-abc"))?;
        assert_eq!(dest.image_id, "win7");
        assert_eq!(dest.version, "sha256-abc");
        assert_eq!(dest.image_root_prefix, "images/win7/");
        assert_eq!(dest.version_prefix, "images/win7/sha256-abc/");
        assert_eq!(
            manifest_object_key(&dest.version_prefix),
            "images/win7/sha256-abc/manifest.json"
        );
        assert_eq!(
            latest_object_key(&dest.image_root_prefix),
            "images/win7/latest.json"
        );
        Ok(())
    }

    #[test]
    fn resolve_publish_destination_rejects_explicit_image_version_mismatch_with_computed() {
        let args = PublishArgs {
            file: PathBuf::from("disk.img"),
            format: InputFormat::Raw,
            bucket: "bucket".to_string(),
            prefix: "images/win7/".to_string(),
            image_id: None,
            image_version: Some("sha256-wrong".to_string()),
            compute_version: ComputeVersion::Sha256,
            publish_latest: false,
            cache_control_chunks: DEFAULT_CACHE_CONTROL_CHUNKS.to_string(),
            cache_control_manifest: DEFAULT_CACHE_CONTROL_MANIFEST.to_string(),
            cache_control_latest: DEFAULT_CACHE_CONTROL_LATEST.to_string(),
            chunk_size: DEFAULT_CHUNK_SIZE_BYTES,
            checksum: ChecksumAlgorithm::Sha256,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: DEFAULT_CONCURRENCY,
            retries: DEFAULT_RETRIES,
            no_meta: false,
        };
        let prefix = normalize_prefix(&args.prefix);
        let err = resolve_publish_destination(&args, &prefix, Some("sha256-abc"))
            .expect_err("expected version mismatch error");
        assert!(
            err.to_string().contains("does not match computed version"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn resolve_publish_destination_accepts_explicit_image_version_match_with_computed() -> Result<()>
    {
        let args = PublishArgs {
            file: PathBuf::from("disk.img"),
            format: InputFormat::Raw,
            bucket: "bucket".to_string(),
            prefix: "images/win7/".to_string(),
            image_id: None,
            image_version: Some("sha256-abc".to_string()),
            compute_version: ComputeVersion::Sha256,
            publish_latest: false,
            cache_control_chunks: DEFAULT_CACHE_CONTROL_CHUNKS.to_string(),
            cache_control_manifest: DEFAULT_CACHE_CONTROL_MANIFEST.to_string(),
            cache_control_latest: DEFAULT_CACHE_CONTROL_LATEST.to_string(),
            chunk_size: DEFAULT_CHUNK_SIZE_BYTES,
            checksum: ChecksumAlgorithm::Sha256,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: DEFAULT_CONCURRENCY,
            retries: DEFAULT_RETRIES,
            no_meta: false,
        };
        let prefix = normalize_prefix(&args.prefix);
        let dest = resolve_publish_destination(&args, &prefix, Some("sha256-abc"))?;
        assert_eq!(dest.version, "sha256-abc");
        assert_eq!(dest.image_id, "win7");
        assert_eq!(dest.version_prefix, "images/win7/sha256-abc/");
        Ok(())
    }

    #[test]
    fn resolve_publish_destination_rejects_versioned_prefix_mismatch_with_computed_version() {
        let inferred_version = sha256_version_from_digest([0u8; 32]);
        let computed_version = sha256_version_from_digest([1u8; 32]);

        let args = PublishArgs {
            file: PathBuf::from("disk.img"),
            format: InputFormat::Raw,
            bucket: "bucket".to_string(),
            prefix: format!("images/win7/{inferred_version}/"),
            image_id: None,
            image_version: None,
            compute_version: ComputeVersion::Sha256,
            publish_latest: false,
            cache_control_chunks: DEFAULT_CACHE_CONTROL_CHUNKS.to_string(),
            cache_control_manifest: DEFAULT_CACHE_CONTROL_MANIFEST.to_string(),
            cache_control_latest: DEFAULT_CACHE_CONTROL_LATEST.to_string(),
            chunk_size: DEFAULT_CHUNK_SIZE_BYTES,
            checksum: ChecksumAlgorithm::Sha256,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: DEFAULT_CONCURRENCY,
            retries: DEFAULT_RETRIES,
            no_meta: false,
        };
        let prefix = normalize_prefix(&args.prefix);
        let err = resolve_publish_destination(&args, &prefix, Some(&computed_version))
            .expect_err("expected prefix/computed version mismatch");
        let msg = err.to_string();
        assert!(
            msg.contains("prefix appears to end with sha256 version")
                && msg.contains(&inferred_version)
                && msg.contains(&computed_version),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn sha256_version_matches_expected() {
        let mut hasher = Sha256::new();
        hasher.update(b"hello ");
        hasher.update(b"world");
        let version = sha256_version_from_digest(hasher.finalize());
        assert_eq!(
            version,
            "sha256-b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[tokio::test]
    async fn compute_image_version_sha256_hashes_virtual_disk_bytes_for_aerosparse() -> Result<()> {
        use std::io::Write;

        use aero_storage::{AeroSparseConfig, AeroSparseDisk, MemBackend};

        let disk_size_bytes = 16 * 1024u64;

        // Create an AeroSparse image whose physical file is smaller than the virtual disk
        // (unallocated tail blocks remain implicit zeros).
        let backend = MemBackend::new();
        let mut disk = AeroSparseDisk::create(
            backend,
            AeroSparseConfig {
                disk_size_bytes,
                block_size_bytes: 4096,
            },
        )?;
        disk.write_at(0, b"hello")?;
        disk.flush()?;

        let mut tmp = tempfile::NamedTempFile::new().context("create tempfile")?;
        tmp.as_file_mut()
            .write_all(&disk.into_backend().into_vec())
            .context("write aerosparse image")?;
        tmp.as_file_mut()
            .flush()
            .context("flush aerosparse image")?;

        let physical_len = tmp.as_file().metadata().context("stat temp image")?.len();
        assert!(
            physical_len < disk_size_bytes,
            "expected aerosparse physical file ({physical_len}) < virtual disk ({disk_size_bytes})"
        );

        // Compute the expected hash from the guest-visible byte stream.
        let mut expected = vec![0u8; disk_size_bytes as usize];
        expected[0..5].copy_from_slice(b"hello");
        let expected_version = format!("sha256-{}", sha256_hex(&expected));

        let version = compute_image_version_sha256(tmp.path(), InputFormat::Auto).await?;
        assert_eq!(version, expected_version);
        Ok(())
    }

    #[tokio::test]
    async fn compute_image_version_sha256_hashes_virtual_disk_bytes_for_vhd_fixed() -> Result<()> {
        use std::io::Write;

        let virtual_size = 64 * 1024u64;
        let mut data = vec![0u8; virtual_size as usize];
        data[0..10].copy_from_slice(b"hello vhd!");

        let mut footer = [0u8; SECTOR_SIZE];
        footer[0..8].copy_from_slice(b"conectix");
        footer[8..12].copy_from_slice(&2u32.to_be_bytes()); // features
        footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // file_format_version
        footer[16..24].copy_from_slice(&u64::MAX.to_be_bytes()); // data_offset for fixed disks
        footer[40..48].copy_from_slice(&virtual_size.to_be_bytes()); // original_size
        footer[48..56].copy_from_slice(&virtual_size.to_be_bytes()); // current_size
        footer[60..64].copy_from_slice(&2u32.to_be_bytes()); // disk_type fixed
                                                             // checksum at 64..68 (big-endian)
        let checksum = {
            let mut sum: u32 = 0;
            for (i, b) in footer.iter().enumerate() {
                if (64..68).contains(&i) {
                    continue;
                }
                sum = sum.wrapping_add(*b as u32);
            }
            !sum
        };
        footer[64..68].copy_from_slice(&checksum.to_be_bytes());

        let mut tmp = tempfile::NamedTempFile::new().context("create tempfile")?;
        tmp.as_file_mut()
            .write_all(&data)
            .context("write vhd data")?;
        tmp.as_file_mut()
            .write_all(&footer)
            .context("write vhd footer")?;
        tmp.as_file_mut().flush().context("flush vhd image")?;

        let physical_len = tmp.as_file().metadata().context("stat temp vhd")?.len();
        assert_eq!(
            physical_len,
            virtual_size + SECTOR_SIZE as u64,
            "expected fixed VHD file len to be data + footer"
        );

        let expected_version = format!("sha256-{}", sha256_hex(&data));
        let version = compute_image_version_sha256(tmp.path(), InputFormat::Auto).await?;
        assert_eq!(version, expected_version);
        Ok(())
    }

    #[tokio::test]
    async fn compute_image_version_sha256_hashes_virtual_disk_bytes_for_vhd_fixed_with_footer_copy(
    ) -> Result<()> {
        use std::io::Write;

        let virtual_size = 64 * 1024u64;
        let mut data = vec![0u8; virtual_size as usize];
        data[0..10].copy_from_slice(b"hello vhd!");

        let mut footer = [0u8; SECTOR_SIZE];
        footer[0..8].copy_from_slice(b"conectix");
        footer[8..12].copy_from_slice(&2u32.to_be_bytes()); // features
        footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // file_format_version
        footer[16..24].copy_from_slice(&u64::MAX.to_be_bytes()); // data_offset for fixed disks
        footer[40..48].copy_from_slice(&virtual_size.to_be_bytes()); // original_size
        footer[48..56].copy_from_slice(&virtual_size.to_be_bytes()); // current_size
        footer[60..64].copy_from_slice(&2u32.to_be_bytes()); // disk_type fixed
                                                             // checksum at 64..68 (big-endian)
        let checksum = {
            let mut sum: u32 = 0;
            for (i, b) in footer.iter().enumerate() {
                if (64..68).contains(&i) {
                    continue;
                }
                sum = sum.wrapping_add(*b as u32);
            }
            !sum
        };
        footer[64..68].copy_from_slice(&checksum.to_be_bytes());

        // Fixed VHD with an optional footer copy at offset 0 (payload begins at offset 512).
        let mut tmp = tempfile::NamedTempFile::new().context("create tempfile")?;
        tmp.as_file_mut()
            .write_all(&footer)
            .context("write vhd footer copy")?;
        tmp.as_file_mut()
            .write_all(&data)
            .context("write vhd payload")?;
        tmp.as_file_mut()
            .write_all(&footer)
            .context("write vhd footer")?;
        tmp.as_file_mut().flush().context("flush vhd image")?;

        let physical_len = tmp.as_file().metadata().context("stat temp vhd")?.len();
        assert_eq!(
            physical_len,
            virtual_size + (SECTOR_SIZE as u64) * 2,
            "expected fixed VHD file len to be footer copy + data + footer"
        );

        let expected_version = format!("sha256-{}", sha256_hex(&data));
        let version = compute_image_version_sha256(tmp.path(), InputFormat::Auto).await?;
        assert_eq!(version, expected_version);
        Ok(())
    }

    #[tokio::test]
    async fn compute_image_version_sha256_hashes_virtual_disk_bytes_for_qcow2() -> Result<()> {
        use std::io::Write;

        use aero_storage::{MemBackend, StorageBackend};

        const QCOW2_OFLAG_COPIED: u64 = 1 << 63;

        fn write_be_u32(buf: &mut [u8], offset: usize, val: u32) {
            buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
        }

        fn write_be_u64(buf: &mut [u8], offset: usize, val: u64) {
            buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
        }

        fn make_qcow2_with_pattern(virtual_size: u64) -> MemBackend {
            assert_eq!(virtual_size % SECTOR_SIZE as u64, 0);

            // Keep fixture small while still exercising the full metadata path.
            let cluster_bits = 12u32; // 4 KiB clusters
            let cluster_size = 1u64 << cluster_bits;

            let refcount_table_offset = cluster_size;
            let l1_table_offset = cluster_size * 2;
            let refcount_block_offset = cluster_size * 3;
            let l2_table_offset = cluster_size * 4;

            let file_len = cluster_size * 5;
            let mut backend = MemBackend::with_len(file_len).unwrap();

            let mut header = [0u8; 104];
            header[0..4].copy_from_slice(b"QFI\xfb");
            write_be_u32(&mut header, 4, 3); // version
            write_be_u32(&mut header, 20, cluster_bits);
            write_be_u64(&mut header, 24, virtual_size);
            write_be_u32(&mut header, 36, 1); // l1_size
            write_be_u64(&mut header, 40, l1_table_offset);
            write_be_u64(&mut header, 48, refcount_table_offset);
            write_be_u32(&mut header, 56, 1); // refcount_table_clusters
            write_be_u64(&mut header, 72, 0); // incompatible_features
            write_be_u64(&mut header, 80, 0); // compatible_features
            write_be_u64(&mut header, 88, 0); // autoclear_features
            write_be_u32(&mut header, 96, 4); // refcount_order (16-bit)
            write_be_u32(&mut header, 100, 104); // header_length
            backend.write_at(0, &header).unwrap();

            // Refcount table points at a single refcount block.
            backend
                .write_at(refcount_table_offset, &refcount_block_offset.to_be_bytes())
                .unwrap();

            // L1 table points at a single L2 table.
            let l1_entry = l2_table_offset | QCOW2_OFLAG_COPIED;
            backend
                .write_at(l1_table_offset, &l1_entry.to_be_bytes())
                .unwrap();

            // Mark metadata clusters as in-use: header, refcount table, L1 table, refcount block, L2 table.
            for cluster_index in 0u64..5 {
                let off = refcount_block_offset + cluster_index * 2;
                backend.write_at(off, &1u16.to_be_bytes()).unwrap();
            }

            // Allocate a single data cluster and map guest cluster 0 to it.
            let data_cluster_offset = cluster_size * 5;
            backend.set_len(cluster_size * 6).unwrap();

            let l2_entry = data_cluster_offset | QCOW2_OFLAG_COPIED;
            backend
                .write_at(l2_table_offset, &l2_entry.to_be_bytes())
                .unwrap();

            // Mark the new data cluster as allocated in the refcount block (cluster index 5).
            backend
                .write_at(refcount_block_offset + 5 * 2, &1u16.to_be_bytes())
                .unwrap();

            let mut sector = [0u8; SECTOR_SIZE];
            sector[..12].copy_from_slice(b"hello qcow2!");
            backend.write_at(data_cluster_offset, &sector).unwrap();

            backend
        }

        let virtual_size = 16 * 1024u64;
        let backend = make_qcow2_with_pattern(virtual_size);

        let mut tmp = tempfile::NamedTempFile::new().context("create tempfile")?;
        tmp.as_file_mut()
            .write_all(&backend.into_vec())
            .context("write qcow2 image")?;
        tmp.as_file_mut().flush().context("flush qcow2 image")?;

        let mut expected = vec![0u8; virtual_size as usize];
        expected[0..12].copy_from_slice(b"hello qcow2!");
        let expected_version = format!("sha256-{}", sha256_hex(&expected));

        let version = compute_image_version_sha256(tmp.path(), InputFormat::Auto).await?;
        assert_eq!(version, expected_version);
        Ok(())
    }

    #[tokio::test]
    async fn compute_image_version_sha256_hashes_virtual_disk_bytes_for_raw() -> Result<()> {
        use std::io::Write;

        let disk_size_bytes = 16 * 1024u64;

        let mut data = vec![0u8; disk_size_bytes as usize];
        data[0..8].copy_from_slice(b"RAWTEST!");
        for (i, b) in data.iter_mut().enumerate().skip(8) {
            *b = (i % 251) as u8;
        }

        let mut tmp = tempfile::NamedTempFile::new().context("create tempfile")?;
        tmp.as_file_mut()
            .write_all(&data)
            .context("write raw image")?;
        tmp.as_file_mut().flush().context("flush raw image")?;

        let expected_version = format!("sha256-{}", sha256_hex(&data));

        // Auto detection should fall back to raw for unknown bytes.
        let version = compute_image_version_sha256(tmp.path(), InputFormat::Auto).await?;
        assert_eq!(version, expected_version);

        // Explicit raw should also match.
        let version = compute_image_version_sha256(tmp.path(), InputFormat::Raw).await?;
        assert_eq!(version, expected_version);
        Ok(())
    }

    #[tokio::test]
    async fn compute_image_version_sha256_hashes_virtual_disk_bytes_for_vhd_dynamic() -> Result<()>
    {
        use std::io::Write;

        use aero_storage::{MemBackend, StorageBackend, VhdDisk};

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

        fn vhd_dynamic_header_checksum(raw: &[u8; 1024]) -> u32 {
            let mut sum: u32 = 0;
            for (i, b) in raw.iter().enumerate() {
                if (36..40).contains(&i) {
                    continue;
                }
                sum = sum.wrapping_add(*b as u32);
            }
            !sum
        }

        fn make_vhd_footer(
            virtual_size: u64,
            disk_type: u32,
            data_offset: u64,
        ) -> [u8; SECTOR_SIZE] {
            let mut footer = [0u8; SECTOR_SIZE];
            footer[0..8].copy_from_slice(b"conectix");
            write_be_u32(&mut footer, 8, 2); // features
            write_be_u32(&mut footer, 12, 0x0001_0000); // file_format_version
            write_be_u64(&mut footer, 16, data_offset);
            write_be_u64(&mut footer, 40, virtual_size); // original_size
            write_be_u64(&mut footer, 48, virtual_size); // current_size
            write_be_u32(&mut footer, 60, disk_type);
            let checksum = vhd_footer_checksum(&footer);
            write_be_u32(&mut footer, 64, checksum);
            footer
        }

        fn make_vhd_dynamic_empty(virtual_size: u64, block_size: u32) -> MemBackend {
            assert_eq!(virtual_size % SECTOR_SIZE as u64, 0);
            assert_eq!(block_size as usize % SECTOR_SIZE, 0);

            let dyn_header_offset = SECTOR_SIZE as u64;
            let table_offset = dyn_header_offset + 1024u64;
            let blocks = virtual_size.div_ceil(block_size as u64);
            let max_table_entries = blocks as u32;
            let bat_bytes = max_table_entries as u64 * 4;
            let bat_size = bat_bytes.div_ceil(SECTOR_SIZE as u64) * SECTOR_SIZE as u64;

            let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
            let file_len = (SECTOR_SIZE as u64) + 1024 + bat_size + (SECTOR_SIZE as u64);
            let mut backend = MemBackend::with_len(file_len).unwrap();

            backend.write_at(0, &footer).unwrap();
            backend
                .write_at(file_len - SECTOR_SIZE as u64, &footer)
                .unwrap();

            let mut dyn_header = [0u8; 1024];
            dyn_header[0..8].copy_from_slice(b"cxsparse");
            write_be_u64(&mut dyn_header, 8, u64::MAX);
            write_be_u64(&mut dyn_header, 16, table_offset);
            write_be_u32(&mut dyn_header, 24, 0x0001_0000);
            write_be_u32(&mut dyn_header, 28, max_table_entries);
            write_be_u32(&mut dyn_header, 32, block_size);
            let checksum = vhd_dynamic_header_checksum(&dyn_header);
            write_be_u32(&mut dyn_header, 36, checksum);
            backend.write_at(dyn_header_offset, &dyn_header).unwrap();

            let bat = vec![0xFFu8; bat_size as usize];
            backend.write_at(table_offset, &bat).unwrap();
            backend
        }

        let virtual_size = 64 * 1024u64;
        let block_size = 16 * 1024u32;
        let backend = make_vhd_dynamic_empty(virtual_size, block_size);

        // Use the vhd implementation to allocate blocks and write a pattern.
        let mut disk = VhdDisk::open(backend)?;
        disk.write_at(0, b"hello vhd-d!")?;
        disk.flush()?;

        let mut tmp = tempfile::NamedTempFile::new().context("create tempfile")?;
        tmp.as_file_mut()
            .write_all(&disk.into_backend().into_vec())
            .context("write vhd-d image")?;
        tmp.as_file_mut().flush().context("flush vhd-d image")?;

        let mut expected = vec![0u8; virtual_size as usize];
        expected[0..12].copy_from_slice(b"hello vhd-d!");
        let expected_version = format!("sha256-{}", sha256_hex(&expected));

        let version = compute_image_version_sha256(tmp.path(), InputFormat::Auto).await?;
        assert_eq!(version, expected_version);
        Ok(())
    }

    #[tokio::test]
    async fn compute_image_version_sha256_rejects_qcow2_backing_file_without_parent() -> Result<()>
    {
        use std::io::Write;

        // Create a minimal QCOW2 v2 header that passes structural validation but declares a backing
        // file reference. The qcow2 implementation only supports backing files when an explicit
        // parent disk is provided (`open_with_parent`), so `DiskImage::open_auto` must reject it.
        let mut header = [0u8; 72];
        header[0..4].copy_from_slice(b"QFI\xfb");
        header[4..8].copy_from_slice(&2u32.to_be_bytes()); // version 2
        header[8..16].copy_from_slice(&72u64.to_be_bytes()); // backing_file_offset
        header[16..20].copy_from_slice(&8u32.to_be_bytes()); // backing_file_size
        header[20..24].copy_from_slice(&12u32.to_be_bytes()); // cluster_bits (4096)
        header[24..32].copy_from_slice(&4096u64.to_be_bytes()); // virtual size
        header[32..36].copy_from_slice(&0u32.to_be_bytes()); // crypt_method
        header[36..40].copy_from_slice(&1u32.to_be_bytes()); // l1_size
        header[40..48].copy_from_slice(&4096u64.to_be_bytes()); // l1_table_offset
        header[48..56].copy_from_slice(&8192u64.to_be_bytes()); // refcount_table_offset
        header[56..60].copy_from_slice(&1u32.to_be_bytes()); // refcount_table_clusters
                                                             // nb_snapshots (0) and snapshots_offset (0) remain zero.

        let mut tmp = tempfile::NamedTempFile::new().context("create tempfile")?;
        tmp.as_file_mut()
            .write_all(&header)
            .context("write qcow2 header")?;
        tmp.as_file_mut().flush().context("flush qcow2 header")?;

        let err = compute_image_version_sha256(tmp.path(), InputFormat::Auto)
            .await
            .expect_err("expected qcow2 backing file rejection");
        let summary = error_chain_summary(&err);
        assert!(
            summary.contains("qcow2 backing file"),
            "unexpected error: {summary}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn compute_image_version_sha256_rejects_vhd_differencing_without_parent() -> Result<()> {
        use std::io::Write;

        // Create a minimal VHD footer for a differencing disk (disk_type=4). VHD differencing
        // disks require an explicit parent disk; the chunker only opens a single file and must
        // reject these images.
        let virtual_size = SECTOR_SIZE as u64;
        let data_offset = SECTOR_SIZE as u64; // dynamic header at offset 512
        let file_len = data_offset + 1024 + SECTOR_SIZE as u64;

        let mut footer = [0u8; SECTOR_SIZE];
        footer[0..8].copy_from_slice(b"conectix");
        footer[8..12].copy_from_slice(&2u32.to_be_bytes()); // features
        footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // file_format_version
        footer[16..24].copy_from_slice(&data_offset.to_be_bytes());
        footer[40..48].copy_from_slice(&virtual_size.to_be_bytes()); // original_size
        footer[48..56].copy_from_slice(&virtual_size.to_be_bytes()); // current_size
        footer[60..64].copy_from_slice(&4u32.to_be_bytes()); // disk_type differencing

        let checksum = {
            let mut sum: u32 = 0;
            for (i, b) in footer.iter().enumerate() {
                if (64..68).contains(&i) {
                    continue;
                }
                sum = sum.wrapping_add(*b as u32);
            }
            !sum
        };
        footer[64..68].copy_from_slice(&checksum.to_be_bytes());

        let mut tmp = tempfile::NamedTempFile::new().context("create tempfile")?;
        tmp.as_file_mut()
            .write_all(&vec![0u8; (file_len - SECTOR_SIZE as u64) as usize])
            .context("write vhd body padding")?;
        tmp.as_file_mut()
            .write_all(&footer)
            .context("write vhd footer")?;
        tmp.as_file_mut().flush().context("flush vhd image")?;

        let err = compute_image_version_sha256(tmp.path(), InputFormat::Auto)
            .await
            .expect_err("expected vhd differencing rejection");
        let summary = error_chain_summary(&err);
        assert!(
            summary.contains("differencing") && summary.contains("parent"),
            "unexpected error: {summary}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn compute_image_version_sha256_rejects_zero_size_disk() -> Result<()> {
        let tmp = tempfile::NamedTempFile::new().context("create tempfile")?;

        let err = compute_image_version_sha256(tmp.path(), InputFormat::Auto)
            .await
            .expect_err("expected hash failure");
        assert!(
            err.to_string().contains("virtual disk size must be > 0"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[test]
    fn chunk_count_rounds_up() {
        assert_eq!(chunk_count(0, 8), 0);
        assert_eq!(chunk_count(1, 8), 1);
        assert_eq!(chunk_count(8, 8), 1);
        assert_eq!(chunk_count(9, 8), 2);
    }

    #[test]
    fn chunk_count_does_not_overflow() {
        // `total_size + chunk_size - 1` can overflow for large values; ensure we handle this
        // correctly.
        assert_eq!(chunk_count(u64::MAX, 2), u64::MAX.div_ceil(2));
    }

    #[test]
    fn cli_default_chunk_size_is_4_mib() {
        let cli = Cli::parse_from([
            "aero-image-chunker",
            "publish",
            "--file",
            "disk.img",
            "--bucket",
            "bucket",
            "--prefix",
            "images/win7/sha256-abc/",
        ]);
        let Commands::Publish(args) = cli.command else {
            panic!("expected publish subcommand");
        };
        assert_eq!(args.chunk_size, DEFAULT_CHUNK_SIZE_BYTES);
        assert_eq!(args.chunk_size, 4 * 1024 * 1024);
        assert!(matches!(args.format, InputFormat::Auto));
    }

    #[tokio::test]
    async fn publish_rejects_non_sector_aligned_chunk_size() {
        let cli = Cli::parse_from([
            "aero-image-chunker",
            "publish",
            "--file",
            "disk.img",
            "--bucket",
            "bucket",
            "--prefix",
            "images/win7/sha256-abc/",
        ]);
        let Commands::Publish(mut args) = cli.command else {
            panic!("expected publish subcommand");
        };
        // Force a non-sector-aligned chunk size.
        args.chunk_size = 1;
        let err = publish(args).await.expect_err("expected publish failure");
        assert!(
            err.to_string().contains("--chunk-size must be a multiple"),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn publish_rejects_zero_chunk_size() {
        let cli = Cli::parse_from([
            "aero-image-chunker",
            "publish",
            "--file",
            "disk.img",
            "--bucket",
            "bucket",
            "--prefix",
            "images/win7/sha256-abc/",
        ]);
        let Commands::Publish(mut args) = cli.command else {
            panic!("expected publish subcommand");
        };
        args.chunk_size = 0;

        let err = publish(args).await.expect_err("expected publish failure");
        assert!(
            err.to_string().contains("--chunk-size must be > 0"),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn publish_rejects_too_large_chunk_size() {
        let cli = Cli::parse_from([
            "aero-image-chunker",
            "publish",
            "--file",
            "disk.img",
            "--bucket",
            "bucket",
            "--prefix",
            "images/win7/sha256-abc/",
        ]);
        let Commands::Publish(mut args) = cli.command else {
            panic!("expected publish subcommand");
        };
        args.chunk_size = MAX_CHUNK_SIZE_BYTES + SECTOR_SIZE as u64;

        let err = publish(args).await.expect_err("expected publish failure");
        assert!(
            err.to_string().contains("--chunk-size too large"),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn publish_rejects_zero_concurrency() {
        let cli = Cli::parse_from([
            "aero-image-chunker",
            "publish",
            "--file",
            "disk.img",
            "--bucket",
            "bucket",
            "--prefix",
            "images/win7/sha256-abc/",
        ]);
        let Commands::Publish(mut args) = cli.command else {
            panic!("expected publish subcommand");
        };
        args.concurrency = 0;

        let err = publish(args).await.expect_err("expected publish failure");
        assert!(
            err.to_string().contains("--concurrency must be > 0"),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn publish_rejects_zero_retries() {
        let cli = Cli::parse_from([
            "aero-image-chunker",
            "publish",
            "--file",
            "disk.img",
            "--bucket",
            "bucket",
            "--prefix",
            "images/win7/sha256-abc/",
        ]);
        let Commands::Publish(mut args) = cli.command else {
            panic!("expected publish subcommand");
        };
        args.retries = 0;

        let err = publish(args).await.expect_err("expected publish failure");
        assert!(
            err.to_string().contains("--retries must be > 0"),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn publish_rejects_too_many_chunks() -> Result<()> {
        // Chunk count compatibility limit is enforced before any network calls. Create a sparse raw
        // image large enough that chunkCount would exceed MAX_COMPAT_CHUNK_COUNT when chunkSize is
        // 512 bytes.
        let total_size = (MAX_COMPAT_CHUNK_COUNT + 1) * (SECTOR_SIZE as u64);

        let mut tmp = tempfile::NamedTempFile::new().context("create tempfile")?;
        tmp.as_file_mut()
            .set_len(total_size)
            .context("resize raw image")?;

        let cli = Cli::parse_from([
            "aero-image-chunker",
            "publish",
            "--file",
            "disk.img",
            "--bucket",
            "bucket",
            "--prefix",
            "images/win7/sha256-abc/",
        ]);
        let Commands::Publish(mut args) = cli.command else {
            panic!("expected publish subcommand");
        };
        args.file = tmp.path().to_path_buf();
        args.format = InputFormat::Raw;
        args.chunk_size = SECTOR_SIZE as u64;

        let err = publish(args).await.expect_err("expected publish failure");
        assert!(
            err.to_string()
                .contains("exceeds the current compatibility limit"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn publish_rejects_non_sector_aligned_virtual_disk_size() -> Result<()> {
        use std::io::Write;

        let mut tmp = tempfile::NamedTempFile::new().context("create tempfile")?;
        tmp.as_file_mut()
            .write_all(&vec![0u8; 1000])
            .context("write raw image")?;
        tmp.as_file_mut().flush().context("flush raw image")?;

        let cli = Cli::parse_from([
            "aero-image-chunker",
            "publish",
            "--file",
            "disk.img",
            "--bucket",
            "bucket",
            "--prefix",
            "images/win7/sha256-abc/",
        ]);
        let Commands::Publish(mut args) = cli.command else {
            panic!("expected publish subcommand");
        };
        args.file = tmp.path().to_path_buf();
        args.format = InputFormat::Raw;

        let err = publish(args).await.expect_err("expected publish failure");
        assert!(
            err.to_string().contains("virtual disk size")
                && err.to_string().contains("not a multiple"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn publish_rejects_zero_virtual_disk_size() -> Result<()> {
        let tmp = tempfile::NamedTempFile::new().context("create tempfile")?;

        let cli = Cli::parse_from([
            "aero-image-chunker",
            "publish",
            "--file",
            "disk.img",
            "--bucket",
            "bucket",
            "--prefix",
            "images/win7/sha256-abc/",
        ]);
        let Commands::Publish(mut args) = cli.command else {
            panic!("expected publish subcommand");
        };
        args.file = tmp.path().to_path_buf();
        args.format = InputFormat::Raw;

        let err = publish(args).await.expect_err("expected publish failure");
        assert!(
            err.to_string().contains("virtual disk size") && err.to_string().contains("> 0"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[test]
    fn publish_help_mentions_default_chunk_size() {
        use clap::CommandFactory;

        let mut cmd = Cli::command();
        let publish = cmd
            .find_subcommand_mut("publish")
            .expect("publish subcommand");
        let help = publish.render_long_help().to_string();
        assert!(
            help.contains(&format!("[default: {}]", DEFAULT_CHUNK_SIZE_BYTES)),
            "publish help did not mention default chunk size; help was:\n{help}"
        );
    }

    #[test]
    fn build_manifest_v1_sets_chunk_count_and_last_chunk_size() -> Result<()> {
        let manifest =
            build_manifest_v1(10, 4, "win7", "sha256-abc", ChecksumAlgorithm::None, &[])?;
        assert_eq!(manifest.total_size, 10);
        assert_eq!(manifest.chunk_size, 4);
        assert_eq!(manifest.chunk_count, 3);
        let chunks = manifest.chunks.expect("chunks present");
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].size, Some(4));
        assert_eq!(chunks[1].size, Some(4));
        assert_eq!(chunks[2].size, Some(2));
        assert_eq!(chunks[0].sha256, None);
        Ok(())
    }

    #[test]
    fn build_manifest_v1_rejects_missing_sha256_entry() {
        let err = build_manifest_v1(
            10,
            4,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &[Some(sha256_hex(b"chunk0"))],
        )
        .expect_err("expected missing sha256 failure");
        assert!(
            err.to_string().contains("missing sha256 for chunk 1"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn build_manifest_v1_rejects_explicit_null_sha256_entry() {
        let err = build_manifest_v1(
            10,
            4,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &[
                Some(sha256_hex(b"chunk0")),
                None,
                Some(sha256_hex(b"chunk2")),
            ],
        )
        .expect_err("expected missing sha256 failure");
        assert!(
            err.to_string().contains("missing sha256 for chunk 1"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_unknown_schema() {
        let manifest = ManifestV1 {
            schema: "not-a-schema".to_string(),
            image_id: "win7".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: 0,
            chunk_size: 512,
            chunk_count: 0,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: Some(Vec::new()),
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected schema validation failure");
        assert!(
            err.to_string().contains("manifest schema mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_empty_image_id() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected imageId validation failure");
        assert!(
            err.to_string()
                .contains("manifest imageId must be non-empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_empty_version() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let err =
            validate_manifest_v1(&manifest, MAX_CHUNKS).expect_err("expected version validation");
        assert!(
            err.to_string()
                .contains("manifest version must be non-empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_empty_mime_type() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: "".to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected mimeType validation failure");
        assert!(
            err.to_string()
                .contains("manifest mimeType must be non-empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_zero_chunk_size() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: 0,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected chunkSize validation failure");
        assert!(
            err.to_string().contains("manifest chunkSize must be > 0"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_zero_chunk_index_width() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: 0,
            chunks: None,
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected chunkIndexWidth validation failure");
        assert!(
            err.to_string()
                .contains("manifest chunkIndexWidth must be > 0"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_chunk_sha256_non_hex() {
        let bad = "z".repeat(64);
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: Some(vec![ManifestChunkV1 {
                size: Some(SECTOR_SIZE as u64),
                sha256: Some(bad),
            }]),
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected sha256 validation failure");
        let msg = error_chain_summary(&err);
        assert!(
            msg.contains("manifest chunk[0].sha256 is invalid") && msg.contains("expected hex"),
            "unexpected error chain: {msg}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_zero_total_size() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: 0,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 0,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected totalSize validation failure");
        assert!(
            err.to_string().contains("manifest totalSize must be > 0"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_non_sector_aligned_total_size() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: 1,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: Some(vec![ManifestChunkV1 {
                size: Some(1),
                sha256: None,
            }]),
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected totalSize alignment validation failure");
        assert!(
            err.to_string()
                .contains("manifest totalSize must be a multiple"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_non_sector_aligned_chunk_size() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: (SECTOR_SIZE as u64) * 2,
            chunk_size: (SECTOR_SIZE as u64) + 1,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected chunkSize alignment validation failure");
        assert!(
            err.to_string()
                .contains("manifest chunkSize must be a multiple"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_chunk_count_mismatch() {
        let total_size = (SECTOR_SIZE as u64) * 3;
        let chunk_size = (SECTOR_SIZE as u64) * 2;
        let expected = chunk_count(total_size, chunk_size);
        assert_eq!(expected, 2);

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: expected + 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected chunkCount mismatch validation failure");
        assert!(
            err.to_string().contains("manifest chunkCount mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_chunk_index_width_too_large() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: 33,
            chunks: None,
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected chunkIndexWidth validation failure");
        assert!(
            err.to_string().contains("unreasonably large"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_chunk_index_width_too_small() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: (SECTOR_SIZE as u64) * 11,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 11,
            chunk_index_width: 1,
            chunks: None,
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected chunkIndexWidth validation failure");
        assert!(
            err.to_string().contains("chunkIndexWidth too small"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_chunk_size_too_large() {
        let big = MAX_CHUNK_SIZE_BYTES + SECTOR_SIZE as u64;
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: big,
            chunk_size: big,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected chunkSize validation failure");
        assert!(
            err.to_string().to_ascii_lowercase().contains("chunksize")
                && err.to_string().to_ascii_lowercase().contains("too large"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_chunk_count_exceeds_max_chunks() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: (SECTOR_SIZE as u64) * 10,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 10,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let err = validate_manifest_v1(&manifest, 5).expect_err("expected max-chunks failure");
        assert!(
            err.to_string().contains("exceeds --max-chunks"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_chunks_length_mismatch() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: (SECTOR_SIZE as u64) * 2,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 2,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: Some(vec![ManifestChunkV1 {
                size: Some(SECTOR_SIZE as u64),
                sha256: None,
            }]),
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected chunks length validation failure");
        assert!(
            err.to_string().contains("manifest chunks length"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_chunk_size_mismatch_in_chunks_list() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: (SECTOR_SIZE as u64) * 3,
            chunk_size: (SECTOR_SIZE as u64) * 2,
            chunk_count: 2,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: Some(vec![
                ManifestChunkV1 {
                    size: Some((SECTOR_SIZE as u64) * 2),
                    sha256: None,
                },
                ManifestChunkV1 {
                    size: Some((SECTOR_SIZE as u64) * 2), // wrong: expected 512
                    sha256: None,
                },
            ]),
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected per-chunk size validation failure");
        assert!(
            err.to_string().contains("manifest chunk[1].size mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_manifest_v1_rejects_invalid_chunk_sha256() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: Some(vec![ManifestChunkV1 {
                size: Some(SECTOR_SIZE as u64),
                sha256: Some("not-hex".to_string()),
            }]),
        };
        let err = validate_manifest_v1(&manifest, MAX_CHUNKS)
            .expect_err("expected sha256 validation failure");
        let msg = error_chain_summary(&err);
        assert!(
            msg.contains("manifest chunk[0].sha256 is invalid") && msg.contains("expected 64 hex"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn validate_meta_matches_manifest_rejects_total_size_mismatch() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let meta = Meta {
            created_at: Utc::now(),
            original_filename: "disk.img".to_string(),
            total_size: manifest.total_size + SECTOR_SIZE as u64,
            chunk_size: manifest.chunk_size,
            chunk_count: manifest.chunk_count,
            checksum_algorithm: ChecksumAlgorithm::Sha256.as_str().to_string(),
        };
        let err = validate_meta_matches_manifest(&meta, &manifest)
            .expect_err("expected validation error");
        assert!(
            err.to_string().contains("meta.json totalSize mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_meta_matches_manifest_rejects_chunk_size_mismatch() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let meta = Meta {
            created_at: Utc::now(),
            original_filename: "disk.img".to_string(),
            total_size: manifest.total_size,
            chunk_size: manifest.chunk_size + SECTOR_SIZE as u64,
            chunk_count: manifest.chunk_count,
            checksum_algorithm: ChecksumAlgorithm::Sha256.as_str().to_string(),
        };
        let err = validate_meta_matches_manifest(&meta, &manifest)
            .expect_err("expected validation error");
        assert!(
            err.to_string().contains("meta.json chunkSize mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_meta_matches_manifest_rejects_chunk_count_mismatch() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let meta = Meta {
            created_at: Utc::now(),
            original_filename: "disk.img".to_string(),
            total_size: manifest.total_size,
            chunk_size: manifest.chunk_size,
            chunk_count: 2,
            checksum_algorithm: ChecksumAlgorithm::Sha256.as_str().to_string(),
        };
        let err = validate_meta_matches_manifest(&meta, &manifest)
            .expect_err("expected validation error");
        assert!(
            err.to_string().contains("meta.json chunkCount mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_latest_v1_accepts_valid_latest() -> Result<()> {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let image_root_prefix = "images/demo/";
        let manifest_key = "images/demo/sha256-abc/manifest.json".to_string();
        let latest = LatestV1 {
            schema: LATEST_SCHEMA.to_string(),
            image_id: manifest.image_id.clone(),
            version: manifest.version.clone(),
            manifest_key: manifest_key.clone(),
        };
        validate_latest_v1(&latest, image_root_prefix, &manifest_key, &manifest)?;
        Ok(())
    }

    #[test]
    fn validate_latest_v1_rejects_schema_mismatch() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let image_root_prefix = "images/demo/";
        let manifest_key = "images/demo/sha256-abc/manifest.json";
        let latest = LatestV1 {
            schema: "not-a-schema".to_string(),
            image_id: manifest.image_id.clone(),
            version: manifest.version.clone(),
            manifest_key: manifest_key.to_string(),
        };
        let err = validate_latest_v1(&latest, image_root_prefix, manifest_key, &manifest)
            .expect_err("expected latest.json schema validation error");
        assert!(
            err.to_string().contains("latest.json schema mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_latest_v1_rejects_image_id_mismatch() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let image_root_prefix = "images/demo/";
        let manifest_key = "images/demo/sha256-abc/manifest.json";
        let latest = LatestV1 {
            schema: LATEST_SCHEMA.to_string(),
            image_id: "other".to_string(),
            version: manifest.version.clone(),
            manifest_key: manifest_key.to_string(),
        };
        let err = validate_latest_v1(&latest, image_root_prefix, manifest_key, &manifest)
            .expect_err("expected latest.json imageId validation error");
        assert!(
            err.to_string().contains("latest.json imageId mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_latest_v1_rejects_manifest_key_mismatch() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let image_root_prefix = "images/demo/";
        let manifest_key = "images/demo/sha256-abc/manifest.json";
        let latest = LatestV1 {
            schema: LATEST_SCHEMA.to_string(),
            image_id: manifest.image_id.clone(),
            version: manifest.version.clone(),
            manifest_key: "images/demo/sha256-abc/not-manifest.json".to_string(),
        };
        let err = validate_latest_v1(&latest, image_root_prefix, manifest_key, &manifest)
            .expect_err("expected latest.json manifestKey validation error");
        assert!(
            err.to_string().contains("latest.json manifestKey mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_latest_v1_rejects_version_mismatch_when_manifest_key_matches_verified() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-real".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let image_root_prefix = "images/demo/";
        let manifest_key = "images/demo/sha256-fake/manifest.json";
        let latest = LatestV1 {
            schema: LATEST_SCHEMA.to_string(),
            image_id: manifest.image_id.clone(),
            version: "sha256-fake".to_string(),
            manifest_key: manifest_key.to_string(),
        };
        let err = validate_latest_v1(&latest, image_root_prefix, manifest_key, &manifest)
            .expect_err("expected latest.json version mismatch");
        assert!(
            err.to_string().contains("latest.json version mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_latest_v1_rejects_manifest_key_mismatch_for_matching_version() {
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-abc".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let image_root_prefix = "images/demo/";
        let verified_manifest_key = "images/demo/sha256-other/manifest.json";
        let latest = LatestV1 {
            schema: LATEST_SCHEMA.to_string(),
            image_id: manifest.image_id.clone(),
            version: manifest.version.clone(),
            manifest_key: "images/demo/sha256-abc/manifest.json".to_string(),
        };
        let err = validate_latest_v1(&latest, image_root_prefix, verified_manifest_key, &manifest)
            .expect_err("expected latest.json manifestKey mismatch");
        assert!(
            err.to_string()
                .contains("latest.json manifestKey mismatch for version"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_latest_v1_allows_pointing_at_different_version() -> Result<()> {
        // This corresponds to verifying an older versioned manifest while `latest.json` has moved
        // on to a newer version. That should be valid as long as latest.json is internally
        // consistent.
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "sha256-old".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let image_root_prefix = "images/demo/";
        let verified_manifest_key = "images/demo/sha256-old/manifest.json";
        let latest = LatestV1 {
            schema: LATEST_SCHEMA.to_string(),
            image_id: manifest.image_id.clone(),
            version: "sha256-new".to_string(),
            manifest_key: "images/demo/sha256-new/manifest.json".to_string(),
        };
        validate_latest_v1(&latest, image_root_prefix, verified_manifest_key, &manifest)?;
        Ok(())
    }

    #[test]
    fn missing_chunk_is_non_retryable_even_with_context_wrapping() {
        let err = anyhow!("object not found (404)");
        let err = Err::<(), _>(err)
            .context("GET s3://bucket/prefix/chunks/00000000.bin")
            .unwrap_err();
        assert!(
            !is_retryable_chunk_error(&err),
            "expected missing chunk to be non-retryable; error chain was: {}",
            error_chain_summary(&err)
        );
    }

    #[test]
    fn size_mismatch_is_non_retryable_even_with_context_wrapping() {
        let err = anyhow!("size mismatch: expected 512 bytes, got 511 bytes (Content-Length)");
        let err = Err::<(), _>(err)
            .context("GET s3://bucket/prefix/chunks/00000000.bin")
            .unwrap_err();
        assert!(
            !is_retryable_chunk_error(&err),
            "expected size mismatch to be non-retryable; error chain was: {}",
            error_chain_summary(&err)
        );
    }

    #[test]
    fn sha256_mismatch_is_non_retryable_even_with_context_wrapping() {
        let err = anyhow!("sha256 mismatch: expected deadbeef, got cafebabe");
        let err = Err::<(), _>(err)
            .context("GET s3://bucket/prefix/chunks/00000000.bin")
            .unwrap_err();
        assert!(
            !is_retryable_chunk_error(&err),
            "expected sha256 mismatch to be non-retryable; error chain was: {}",
            error_chain_summary(&err)
        );
    }

    #[test]
    fn http_404_is_non_retryable() -> Result<()> {
        let url: reqwest::Url = "http://127.0.0.1/manifest.json".parse()?;
        let err = anyhow!(HttpStatusFailure {
            url,
            status: reqwest::StatusCode::NOT_FOUND,
        })
        .context("GET http://127.0.0.1/manifest.json");
        assert!(!is_retryable_http_error(&err));
        Ok(())
    }

    #[test]
    fn http_500_is_retryable() -> Result<()> {
        let url: reqwest::Url = "http://127.0.0.1/manifest.json".parse()?;
        let err = anyhow!(HttpStatusFailure {
            url,
            status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        })
        .context("GET http://127.0.0.1/manifest.json");
        assert!(is_retryable_http_error(&err));
        Ok(())
    }

    #[test]
    fn http_429_is_retryable() -> Result<()> {
        let url: reqwest::Url = "http://127.0.0.1/manifest.json".parse()?;
        let err = anyhow!(HttpStatusFailure {
            url,
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
        })
        .context("GET http://127.0.0.1/manifest.json");
        assert!(is_retryable_http_error(&err));
        Ok(())
    }

    #[test]
    fn http_408_is_retryable() -> Result<()> {
        let url: reqwest::Url = "http://127.0.0.1/manifest.json".parse()?;
        let err = anyhow!(HttpStatusFailure {
            url,
            status: reqwest::StatusCode::REQUEST_TIMEOUT,
        })
        .context("GET http://127.0.0.1/manifest.json");
        assert!(is_retryable_http_error(&err));
        Ok(())
    }

    #[test]
    fn http_size_mismatch_is_non_retryable_even_when_wrapped() {
        let err = anyhow!(
            "size mismatch for chunk 0 (http://127.0.0.1/chunks/00000000.bin): expected 512 bytes, got 511 bytes"
        );
        let err = Err::<(), _>(err)
            .context("chunk verify failed")
            .unwrap_err();
        assert!(!is_retryable_http_error(&err));
    }

    #[test]
    fn http_sha256_mismatch_is_non_retryable_even_when_wrapped() {
        let err = anyhow!(
            "sha256 mismatch for chunk 0 (http://127.0.0.1/chunks/00000000.bin): expected deadbeef, got cafebabe"
        );
        let err = Err::<(), _>(err)
            .context("chunk verify failed")
            .unwrap_err();
        assert!(!is_retryable_http_error(&err));
    }

    #[test]
    fn http_unexpected_content_encoding_is_non_retryable_even_when_wrapped() {
        let err = anyhow!("unexpected Content-Encoding: gzip");
        let err = Err::<(), _>(err)
            .context("GET http://127.0.0.1/chunks/00000000.bin")
            .unwrap_err();
        assert!(!is_retryable_http_error(&err));
    }

    #[test]
    fn http_response_too_large_is_non_retryable_even_when_wrapped() {
        let err = anyhow!("response too large: max 1024 bytes, got 2048 (Content-Length)");
        let err = Err::<(), _>(err)
            .context("GET http://127.0.0.1/manifest.json")
            .unwrap_err();
        assert!(!is_retryable_http_error(&err));
    }

    #[test]
    fn unexpected_content_encoding_is_non_retryable_even_with_context_wrapping() {
        let err =
            anyhow!("unexpected Content-Encoding for s3://bucket/prefix/chunks/00000000.bin: gzip");
        let err = Err::<(), _>(err)
            .context("GET s3://bucket/prefix/chunks/00000000.bin")
            .unwrap_err();
        assert!(
            !is_retryable_chunk_error(&err),
            "expected content-encoding mismatch to be non-retryable; error chain was: {}",
            error_chain_summary(&err)
        );
    }

    #[test]
    fn access_denied_is_non_retryable_even_with_context_wrapping() {
        let err = anyhow!("access denied (403): AccessDenied");
        let err = Err::<(), _>(err)
            .context("GET s3://bucket/prefix/chunks/00000000.bin")
            .unwrap_err();
        assert!(
            !is_retryable_chunk_error(&err),
            "expected access denied to be non-retryable; error chain was: {}",
            error_chain_summary(&err)
        );
    }

    #[test]
    fn sampled_chunk_indices_include_last_and_are_unique() -> Result<()> {
        let chunk_count = 100;
        let sample = 5;
        let mut rng = fastrand::Rng::with_seed(123);
        let indices = select_sampled_chunk_indices(chunk_count, sample, &mut rng)?;
        assert_eq!(indices.len(), (sample + 1) as usize);
        assert_eq!(indices.last().copied(), Some(chunk_count - 1));
        assert!(
            indices.windows(2).all(|w| w[0] < w[1]),
            "indices not sorted/unique"
        );
        assert!(
            indices.iter().all(|&idx| idx < chunk_count),
            "index out of range"
        );
        Ok(())
    }

    #[test]
    fn sampled_chunk_indices_can_be_deterministic_with_seed() -> Result<()> {
        let chunk_count = 123;
        let sample = 8;

        let mut rng1 = fastrand::Rng::with_seed(42);
        let mut rng2 = fastrand::Rng::with_seed(42);
        let a = select_sampled_chunk_indices(chunk_count, sample, &mut rng1)?;
        let b = select_sampled_chunk_indices(chunk_count, sample, &mut rng2)?;
        assert_eq!(a, b);
        Ok(())
    }

    #[test]
    fn sampled_chunk_indices_allow_zero_sample() -> Result<()> {
        let chunk_count = 10;
        let sample = 0;
        let mut rng = fastrand::Rng::with_seed(1);
        let indices = select_sampled_chunk_indices(chunk_count, sample, &mut rng)?;
        assert_eq!(indices, vec![chunk_count - 1]);
        Ok(())
    }

    #[test]
    fn sampled_chunk_indices_handle_empty_and_singleton_images() -> Result<()> {
        let mut rng = fastrand::Rng::with_seed(1);
        assert_eq!(
            select_sampled_chunk_indices(0, 5, &mut rng)?,
            Vec::<u64>::new()
        );
        assert_eq!(select_sampled_chunk_indices(1, 5, &mut rng)?, vec![0]);
        Ok(())
    }

    #[test]
    fn sampled_chunk_indices_return_all_when_sample_covers_population() -> Result<()> {
        let chunk_count = 10;
        let sample = 100;
        let mut rng = fastrand::Rng::with_seed(1);
        let indices = select_sampled_chunk_indices(chunk_count, sample, &mut rng)?;
        assert_eq!(indices, (0..chunk_count).collect::<Vec<_>>());
        Ok(())
    }

    #[test]
    fn manifest_v1_parses_from_json_and_validates() -> Result<()> {
        let json = r#"
{
  "schema": "aero.chunked-disk-image.v1",
  "imageId": "demo",
  "version": "sha256-abc",
  "mimeType": "application/octet-stream",
  "totalSize": 2560,
  "chunkSize": 1024,
  "chunkCount": 3,
  "chunkIndexWidth": 8,
  "chunks": [
    { "size": 1024, "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
    { },
    { }
  ]
}
"#;
        let manifest: ManifestV1 = serde_json::from_str(json)?;
        assert_eq!(manifest.image_id, "demo");
        let chunks = manifest.chunks.as_ref().expect("chunks present");
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].size, Some(1024));
        assert_eq!(chunks[1].size, None);
        assert_eq!(chunks[2].size, None);
        assert_eq!(
            chunks[0].sha256.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(chunks[1].sha256, None);

        validate_manifest_v1(&manifest, MAX_CHUNKS)?;
        Ok(())
    }

    fn dummy_s3_verify_args() -> VerifyArgs {
        VerifyArgs {
            manifest_url: None,
            manifest_file: None,
            header: Vec::new(),
            bucket: Some("bucket".to_string()),
            prefix: Some("images/demo/sha256-abc/".to_string()),
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        }
    }

    #[test]
    fn validate_verify_args_rejects_header_without_manifest_url() {
        let args = VerifyArgs {
            manifest_url: None,
            manifest_file: Some(PathBuf::from("manifest.json")),
            header: vec!["authorization: bearer test".to_string()],
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        };
        let err = validate_verify_args(&args).expect_err("expected validation failure");
        assert!(
            err.to_string()
                .contains("--header can only be used with --manifest-url"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_verify_args_rejects_manifest_url_with_s3_options() {
        let args = VerifyArgs {
            manifest_url: Some("http://example.com/manifest.json".to_string()),
            manifest_file: None,
            header: Vec::new(),
            bucket: Some("bucket".to_string()),
            prefix: Some("images/demo/".to_string()),
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        };
        let err = validate_verify_args(&args).expect_err("expected validation failure");
        assert!(
            err.to_string()
                .contains("--manifest-url/--manifest-file cannot be combined with S3 options"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_verify_args_rejects_missing_bucket_when_no_manifest() {
        let args = VerifyArgs {
            bucket: None,
            prefix: None,
            manifest_key: None,
            ..dummy_s3_verify_args()
        };
        let err = validate_verify_args(&args).expect_err("expected validation failure");
        assert!(
            err.to_string()
                .contains("either --manifest-url/--manifest-file or --bucket is required"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_verify_args_rejects_missing_prefix_and_manifest_key() {
        let args = VerifyArgs {
            bucket: Some("bucket".to_string()),
            prefix: None,
            manifest_key: None,
            ..dummy_s3_verify_args()
        };
        let err = validate_verify_args(&args).expect_err("expected validation failure");
        assert!(
            err.to_string()
                .contains("--prefix or --manifest-key is required with --bucket"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_verify_args_rejects_max_chunks_exceeding_limit() {
        let args = VerifyArgs {
            max_chunks: MAX_CHUNKS + 1,
            ..dummy_s3_verify_args()
        };
        let err = validate_verify_args(&args).expect_err("expected validation failure");
        assert!(
            err.to_string().contains("--max-chunks cannot exceed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_verify_args_rejects_zero_concurrency() {
        let args = VerifyArgs {
            concurrency: 0,
            ..dummy_s3_verify_args()
        };
        let err = validate_verify_args(&args).expect_err("expected validation failure");
        assert!(
            err.to_string().contains("--concurrency must be > 0"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_verify_args_rejects_zero_retries() {
        let args = VerifyArgs {
            retries: 0,
            ..dummy_s3_verify_args()
        };
        let err = validate_verify_args(&args).expect_err("expected validation failure");
        assert!(
            err.to_string().contains("--retries must be > 0"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_verify_args_rejects_header_in_s3_mode() {
        let args = VerifyArgs {
            header: vec!["authorization: bearer test".to_string()],
            ..dummy_s3_verify_args()
        };
        let err = validate_verify_args(&args).expect_err("expected validation failure");
        assert!(
            err.to_string()
                .contains("--header is only valid with --manifest-url"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_header_rejects_missing_colon() {
        let err = parse_header("not-a-header").expect_err("expected parse failure");
        assert!(
            err.to_string().contains("invalid header"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_header_rejects_invalid_header_name() {
        let err = parse_header("Bad Header: value").expect_err("expected parse failure");
        assert!(
            err.to_string()
                .to_ascii_lowercase()
                .contains("invalid header name"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_header_rejects_invalid_header_value() {
        // Newlines are not permitted in header values (would be header injection).
        let err = parse_header("X-Test: ok\r\nInjected: bad").expect_err("expected parse failure");
        assert!(
            err.to_string()
                .to_ascii_lowercase()
                .contains("invalid header value"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_verify_manifest_key_prefers_explicit_manifest_key() -> Result<()> {
        let args = VerifyArgs {
            prefix: None,
            manifest_key: Some("images/demo/sha256-abc/manifest.json".to_string()),
            ..dummy_s3_verify_args()
        };
        assert_eq!(
            resolve_verify_manifest_key(&args)?,
            "images/demo/sha256-abc/manifest.json"
        );
        Ok(())
    }

    #[test]
    fn resolve_verify_manifest_key_uses_prefix_when_already_versioned() -> Result<()> {
        let args = VerifyArgs {
            prefix: Some("images/demo/sha256-abc/".to_string()),
            manifest_key: None,
            image_version: None,
            ..dummy_s3_verify_args()
        };
        assert_eq!(
            resolve_verify_manifest_key(&args)?,
            "images/demo/sha256-abc/manifest.json"
        );
        Ok(())
    }

    #[test]
    fn resolve_verify_manifest_key_appends_image_version_to_image_root_prefix() -> Result<()> {
        let args = VerifyArgs {
            prefix: Some("images/demo/".to_string()),
            manifest_key: None,
            image_version: Some("sha256-abc".to_string()),
            ..dummy_s3_verify_args()
        };
        assert_eq!(
            resolve_verify_manifest_key(&args)?,
            "images/demo/sha256-abc/manifest.json"
        );
        Ok(())
    }

    #[test]
    fn resolve_verify_manifest_key_rejects_missing_prefix() {
        let args = VerifyArgs {
            prefix: None,
            manifest_key: None,
            ..dummy_s3_verify_args()
        };
        let err = resolve_verify_manifest_key(&args).expect_err("expected error");
        assert!(
            err.to_string()
                .contains("--prefix is required when --manifest-key is not provided"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn chunk_sample_seed_requires_chunk_sample_flag() {
        let err = Cli::try_parse_from([
            "aero-image-chunker",
            "verify",
            "--bucket",
            "bucket",
            "--prefix",
            "images/demo/",
            "--chunk-sample-seed",
            "123",
        ])
        .expect_err("expected clap to reject --chunk-sample-seed without --chunk-sample");
        assert!(
            err.to_string().contains("--chunk-sample"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn verify_http_manifest_url_rejects_image_id_mismatch() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
            "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
            _ => (404, Vec::new(), b"not found".to_vec()),
        });
        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: Some("wrong".to_string()),
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        let err = result.expect_err("expected verify failure");
        assert!(
            err.to_string().contains("manifest imageId mismatch"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_manifest_url_rejects_image_version_mismatch() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
            "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
            _ => (404, Vec::new(), b"not found".to_vec()),
        });
        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: Some("wrong".to_string()),
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        let err = result.expect_err("expected verify failure");
        assert!(
            err.to_string().contains("manifest version mismatch"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_manifest_url_rejects_meta_mismatch() -> Result<()> {
        use std::sync::Mutex;

        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let meta = Meta {
            created_at: Utc::now(),
            original_filename: "disk.img".to_string(),
            total_size,
            chunk_size,
            chunk_count: manifest.chunk_count + 1,
            checksum_algorithm: "sha256".to_string(),
        };
        let meta_bytes = serde_json::to_vec_pretty(&meta).context("serialize meta")?;

        let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let requests_for_responder = Arc::clone(&requests);

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| {
            requests_for_responder
                .lock()
                .expect("lock requests")
                .push(req.path.clone());
            match req.path.as_str() {
                "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                "/meta.json" => (200, Vec::new(), meta_bytes.clone()),
                // Chunks should never be requested when meta.json validation fails.
                _ if req.path.starts_with("/chunks/") => (500, Vec::new(), b"unexpected".to_vec()),
                _ => (404, Vec::new(), b"not found".to_vec()),
            }
        });
        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        let err = result.expect_err("expected verify failure");
        let summary = error_chain_summary(&err);
        assert!(
            summary.contains("meta.json chunkCount mismatch"),
            "unexpected error chain: {summary}"
        );
        let requests = requests.lock().expect("lock requests");
        assert!(
            requests.iter().any(|p| p == "/meta.json"),
            "expected meta.json to be requested, got {requests:?}"
        );
        assert!(
            !requests.iter().any(|p| p.starts_with("/chunks/")),
            "expected no chunk fetches on meta mismatch, got {requests:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_rejects_image_id_mismatch() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        let manifest_path = dir.path().join("manifest.json");

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        let err = verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: Some("wrong".to_string()),
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await
        .expect_err("expected verify failure");

        assert!(
            err.to_string().contains("manifest imageId mismatch"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_rejects_image_version_mismatch() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        let manifest_path = dir.path().join("manifest.json");

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        let err = verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: Some("wrong".to_string()),
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await
        .expect_err("expected verify failure");

        assert!(
            err.to_string().contains("manifest version mismatch"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_rejects_oversized_manifest_json() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        let manifest_path = dir.path().join("manifest.json");
        std::fs::File::create(&manifest_path)
            .with_context(|| format!("create {}", manifest_path.display()))?
            .set_len((MAX_MANIFEST_JSON_BYTES as u64) + 1)
            .with_context(|| format!("set_len for {}", manifest_path.display()))?;

        let err = verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await
        .expect_err("expected verify failure");
        assert!(
            err.to_string().contains("manifest file") && err.to_string().contains("too large"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_rejects_oversized_meta_json() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        let manifest_path = dir.path().join("manifest.json");
        let meta_path = dir.path().join("meta.json");

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size: SECTOR_SIZE as u64,
            chunk_size: SECTOR_SIZE as u64,
            chunk_count: 1,
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        std::fs::File::create(&meta_path)
            .with_context(|| format!("create {}", meta_path.display()))?
            .set_len((MAX_MANIFEST_JSON_BYTES as u64) + 1)
            .with_context(|| format!("set_len for {}", meta_path.display()))?;

        let err = verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await
        .expect_err("expected verify failure");
        assert!(
            error_chain_summary(&err).contains("meta.json")
                && error_chain_summary(&err).contains("too large"),
            "unexpected error chain: {}",
            error_chain_summary(&err)
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_and_chunks_succeeds() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        tokio::fs::create_dir_all(dir.path().join("chunks"))
            .await
            .context("create chunks dir")?;

        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; SECTOR_SIZE];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let chunk0_path = dir.path().join(chunk_object_key(0)?);
        let chunk1_path = dir.path().join(chunk_object_key(1)?);
        tokio::fs::write(&chunk0_path, &chunk0)
            .await
            .with_context(|| format!("write {}", chunk0_path.display()))?;
        tokio::fs::write(&chunk1_path, &chunk1)
            .await
            .with_context(|| format!("write {}", chunk1_path.display()))?;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_path = dir.path().join("manifest.json");
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: DEFAULT_RETRIES,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_respects_chunk_index_width() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        tokio::fs::create_dir_all(dir.path().join("chunks"))
            .await
            .context("create chunks dir")?;

        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; SECTOR_SIZE];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let mut manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        manifest.chunk_index_width = 1;

        let chunk0_path = dir.path().join(chunk_object_key_with_width(0, 1)?);
        let chunk1_path = dir.path().join(chunk_object_key_with_width(1, 1)?);
        tokio::fs::write(&chunk0_path, &chunk0)
            .await
            .with_context(|| format!("write {}", chunk0_path.display()))?;
        tokio::fs::write(&chunk1_path, &chunk1)
            .await
            .with_context(|| format!("write {}", chunk1_path.display()))?;

        let manifest_path = dir.path().join("manifest.json");
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_rejects_meta_mismatch() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;

        let chunk_size: u64 = 1024;
        let total_size = chunk_size + (SECTOR_SIZE as u64);

        let sha256_by_index = vec![Some(sha256_hex(b"chunk0")), Some(sha256_hex(b"chunk1"))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_path = dir.path().join("manifest.json");
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        let meta = Meta {
            created_at: Utc::now(),
            original_filename: "disk.img".to_string(),
            total_size,
            chunk_size,
            chunk_count: manifest.chunk_count + 1,
            checksum_algorithm: "sha256".to_string(),
        };
        let meta_path = dir.path().join("meta.json");
        tokio::fs::write(&meta_path, serde_json::to_vec_pretty(&meta)?)
            .await
            .with_context(|| format!("write {}", meta_path.display()))?;

        let err = verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await
        .expect_err("expected verify failure");

        let summary = error_chain_summary(&err);
        assert!(
            summary.contains("meta.json chunkCount mismatch"),
            "unexpected error chain: {summary}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_without_chunks_list_succeeds() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        tokio::fs::create_dir_all(dir.path().join("chunks"))
            .await
            .context("create chunks dir")?;

        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        // Use a final chunk size that is still sector-aligned but not equal to chunk_size.
        let chunk1 = vec![b'b'; SECTOR_SIZE];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let chunk0_path = dir.path().join(chunk_object_key(0)?);
        let chunk1_path = dir.path().join(chunk_object_key(1)?);
        tokio::fs::write(&chunk0_path, &chunk0)
            .await
            .with_context(|| format!("write {}", chunk0_path.display()))?;
        tokio::fs::write(&chunk1_path, &chunk1)
            .await
            .with_context(|| format!("write {}", chunk1_path.display()))?;

        // Omit the per-chunk list entirely; verify should fall back to computed chunk sizing and
        // skip sha256 validation.
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_path = dir.path().join("manifest.json");
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: DEFAULT_RETRIES,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_without_chunks_list_detects_size_mismatch() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        tokio::fs::create_dir_all(dir.path().join("chunks"))
            .await
            .context("create chunks dir")?;

        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; (chunk_size as usize) - 1]; // wrong length
        let chunk1 = vec![b'b'; SECTOR_SIZE];
        let total_size = chunk_size + SECTOR_SIZE as u64;

        let chunk0_path = dir.path().join(chunk_object_key(0)?);
        let chunk1_path = dir.path().join(chunk_object_key(1)?);
        tokio::fs::write(&chunk0_path, &chunk0)
            .await
            .with_context(|| format!("write {}", chunk0_path.display()))?;
        tokio::fs::write(&chunk1_path, &chunk1)
            .await
            .with_context(|| format!("write {}", chunk1_path.display()))?;

        // Omit the per-chunk list entirely; verify should derive expected chunk sizes and fail when
        // a file has the wrong length.
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_path = dir.path().join("manifest.json");
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        let err = verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: DEFAULT_RETRIES,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await
        .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("size mismatch") && msg.contains("chunk 0"),
            "unexpected error message: {msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_with_chunks_list_missing_sizes_succeeds() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        tokio::fs::create_dir_all(dir.path().join("chunks"))
            .await
            .context("create chunks dir")?;

        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; SECTOR_SIZE];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let chunk0_path = dir.path().join(chunk_object_key(0)?);
        let chunk1_path = dir.path().join(chunk_object_key(1)?);
        tokio::fs::write(&chunk0_path, &chunk0)
            .await
            .with_context(|| format!("write {}", chunk0_path.display()))?;
        tokio::fs::write(&chunk1_path, &chunk1)
            .await
            .with_context(|| format!("write {}", chunk1_path.display()))?;

        // Provide per-chunk sha256 but omit per-chunk size fields; verify should derive sizes from
        // totalSize/chunkSize.
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: Some(vec![
                ManifestChunkV1 {
                    size: None,
                    sha256: Some(sha256_hex(&chunk0)),
                },
                ManifestChunkV1 {
                    size: None,
                    sha256: Some(sha256_hex(&chunk1)),
                },
            ]),
        };

        let manifest_path = dir.path().join("manifest.json");
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: DEFAULT_RETRIES,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;

        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_with_chunks_list_missing_sha256_succeeds() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        tokio::fs::create_dir_all(dir.path().join("chunks"))
            .await
            .context("create chunks dir")?;

        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; SECTOR_SIZE];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let chunk0_path = dir.path().join(chunk_object_key(0)?);
        let chunk1_path = dir.path().join(chunk_object_key(1)?);
        tokio::fs::write(&chunk0_path, &chunk0)
            .await
            .with_context(|| format!("write {}", chunk0_path.display()))?;
        tokio::fs::write(&chunk1_path, &chunk1)
            .await
            .with_context(|| format!("write {}", chunk1_path.display()))?;

        // Provide per-chunk size for both chunks, but omit sha256 for chunk 1. Verifier should
        // skip sha256 validation for that chunk and still succeed.
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: Some(vec![
                ManifestChunkV1 {
                    size: Some(chunk0.len() as u64),
                    sha256: Some(sha256_hex(&chunk0)),
                },
                ManifestChunkV1 {
                    size: Some(chunk1.len() as u64),
                    sha256: None,
                },
            ]),
        };

        let manifest_path = dir.path().join("manifest.json");
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: DEFAULT_RETRIES,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;

        Ok(())
    }

    #[tokio::test]
    async fn verify_detects_corrupted_chunk() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        tokio::fs::create_dir_all(dir.path().join("chunks"))
            .await
            .context("create chunks dir")?;

        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; SECTOR_SIZE];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let chunk0_path = dir.path().join(chunk_object_key(0)?);
        let chunk1_path = dir.path().join(chunk_object_key(1)?);
        tokio::fs::write(&chunk0_path, &chunk0)
            .await
            .with_context(|| format!("write {}", chunk0_path.display()))?;
        tokio::fs::write(&chunk1_path, &chunk1)
            .await
            .with_context(|| format!("write {}", chunk1_path.display()))?;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_path = dir.path().join("manifest.json");
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        // Flip a byte but keep the same size so only sha256 verification trips.
        let mut corrupted = chunk0.clone();
        corrupted[0] ^= 0xff;
        tokio::fs::write(&chunk0_path, &corrupted)
            .await
            .with_context(|| format!("corrupt {}", chunk0_path.display()))?;

        let err = verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: DEFAULT_RETRIES,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("sha256 mismatch") && msg.contains("chunk 0"),
            "unexpected error message: {msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_local_manifest_fails_fast_without_summary() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        tokio::fs::create_dir_all(dir.path().join("chunks"))
            .await
            .context("create chunks dir")?;

        let chunk_size: u64 = 1024;
        let chunk1 = vec![b'b'; 512];
        let total_size = chunk_size + (chunk1.len() as u64);

        // Only chunk 1 exists; chunk 0 is missing.
        let chunk1_path = dir.path().join(chunk_object_key(1)?);
        tokio::fs::write(&chunk1_path, &chunk1)
            .await
            .with_context(|| format!("write {}", chunk1_path.display()))?;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };

        let manifest_path = dir.path().join("manifest.json");
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .await
            .with_context(|| format!("write {}", manifest_path.display()))?;

        let err = verify(VerifyArgs {
            manifest_url: None,
            manifest_file: Some(manifest_path),
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await
        .expect_err("expected verify failure");

        let msg = err.to_string();
        assert!(
            msg.contains("chunk 0"),
            "expected error mentioning chunk 0; got: {msg}"
        );
        assert!(
            !msg.contains("verification failed with"),
            "expected fail-fast error without failure summary; got: {msg}"
        );

        Ok(())
    }

    #[tokio::test]
    async fn verify_http_manifest_url_and_chunks_succeeds() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
            "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
            "/chunks/00000000.bin" => (200, Vec::new(), chunk0.clone()),
            "/chunks/00000001.bin" => (200, Vec::new(), chunk1.clone()),
            _ => (404, Vec::new(), b"not found".to_vec()),
        });

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 2,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result
    }

    #[tokio::test]
    async fn verify_http_manifest_url_respects_chunk_index_width() -> Result<()> {
        // Use a minimal chunkIndexWidth to ensure the verifier does not assume fixed-width chunk keys.
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let mut manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        manifest.chunk_index_width = 1;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
            "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
            "/chunks/0.bin" => (200, Vec::new(), chunk0.clone()),
            "/chunks/1.bin" => (200, Vec::new(), chunk1.clone()),
            _ => (404, Vec::new(), b"not found".to_vec()),
        });

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result
    }

    #[tokio::test]
    async fn verify_http_manifest_url_detects_size_mismatch() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; (chunk_size as usize) - 1]; // wrong length
        let chunk1 = vec![b'b'; 512];
        let total_size = chunk_size + 512;

        // Omit per-chunk list; verify should derive sizes and fail on a Content-Length mismatch.
        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
            "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
            "/chunks/00000000.bin" => (200, Vec::new(), chunk0.clone()),
            "/chunks/00000001.bin" => (200, Vec::new(), chunk1.clone()),
            _ => (404, Vec::new(), b"not found".to_vec()),
        });

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        let err = result.expect_err("expected verify failure");
        let msg = err.to_string();
        assert!(
            msg.contains("size mismatch") && msg.contains("chunk 0"),
            "unexpected error message: {msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_manifest_url_with_chunks_list_missing_sizes_succeeds() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: Some(vec![
                ManifestChunkV1 {
                    size: None,
                    sha256: Some(sha256_hex(&chunk0)),
                },
                ManifestChunkV1 {
                    size: None,
                    sha256: Some(sha256_hex(&chunk1)),
                },
            ]),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
            "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
            "/chunks/00000000.bin" => (200, Vec::new(), chunk0.clone()),
            "/chunks/00000001.bin" => (200, Vec::new(), chunk1.clone()),
            _ => (404, Vec::new(), b"not found".to_vec()),
        });

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result
    }

    #[tokio::test]
    async fn verify_http_manifest_url_with_chunks_list_missing_sha256_succeeds() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: Some(vec![
                ManifestChunkV1 {
                    size: Some(chunk0.len() as u64),
                    sha256: Some(sha256_hex(&chunk0)),
                },
                ManifestChunkV1 {
                    size: Some(chunk1.len() as u64),
                    sha256: None,
                },
            ]),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
            "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
            "/chunks/00000000.bin" => (200, Vec::new(), chunk0.clone()),
            "/chunks/00000001.bin" => (200, Vec::new(), chunk1.clone()),
            _ => (404, Vec::new(), b"not found".to_vec()),
        });

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result
    }

    #[tokio::test]
    async fn verify_http_manifest_url_without_chunks_list_succeeds() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
            "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
            "/chunks/00000000.bin" => (200, Vec::new(), chunk0.clone()),
            "/chunks/00000001.bin" => (200, Vec::new(), chunk1.clone()),
            _ => (404, Vec::new(), b"not found".to_vec()),
        });

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result
    }

    #[tokio::test]
    async fn verify_http_uses_head_for_chunks_without_sha256() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let chunk_head_requests = Arc::new(AtomicU64::new(0));
        let chunk_get_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let chunk_head_requests = Arc::clone(&chunk_head_requests);
            let chunk_get_requests = Arc::clone(&chunk_get_requests);
            Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
                "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                "/meta.json" => (404, Vec::new(), b"not found".to_vec()),
                "/chunks/00000000.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (
                            200,
                            vec![("Content-Length".to_string(), chunk0.len().to_string())],
                            Vec::new(),
                        )
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk0.clone())
                    }
                }
                "/chunks/00000001.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (
                            200,
                            vec![("Content-Length".to_string(), chunk1.len().to_string())],
                            Vec::new(),
                        )
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk1.clone())
                    }
                }
                _ => (404, Vec::new(), b"not found".to_vec()),
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result?;
        assert_eq!(
            chunk_get_requests.load(Ordering::SeqCst),
            0,
            "expected chunk bodies to not be downloaded when sha256 is missing and HEAD supplies Content-Length"
        );
        assert_eq!(chunk_head_requests.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_uses_range_get_when_head_is_unsupported() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let chunk_head_requests = Arc::new(AtomicU64::new(0));
        let chunk_range_get_requests = Arc::new(AtomicU64::new(0));
        let chunk_non_range_get_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let chunk_head_requests = Arc::clone(&chunk_head_requests);
            let chunk_range_get_requests = Arc::clone(&chunk_range_get_requests);
            let chunk_non_range_get_requests = Arc::clone(&chunk_non_range_get_requests);
            Arc::new(move |req: TestHttpRequest| {
                let range_header = req
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("range"))
                    .map(|(_, v)| v.as_str());
                match req.path.as_str() {
                    "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                    "/meta.json" => (404, Vec::new(), b"not found".to_vec()),
                    "/chunks/00000000.bin" => {
                        if req.method.eq_ignore_ascii_case("HEAD") {
                            chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                            (405, Vec::new(), Vec::new())
                        } else if range_header == Some("bytes=0-0") {
                            chunk_range_get_requests.fetch_add(1, Ordering::SeqCst);
                            (
                                206,
                                vec![(
                                    "Content-Range".to_string(),
                                    format!("bytes 0-0/{}", chunk0.len()),
                                )],
                                vec![chunk0[0]],
                            )
                        } else {
                            chunk_non_range_get_requests.fetch_add(1, Ordering::SeqCst);
                            (400, Vec::new(), b"missing range".to_vec())
                        }
                    }
                    "/chunks/00000001.bin" => {
                        if req.method.eq_ignore_ascii_case("HEAD") {
                            chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                            (405, Vec::new(), Vec::new())
                        } else if range_header == Some("bytes=0-0") {
                            chunk_range_get_requests.fetch_add(1, Ordering::SeqCst);
                            (
                                206,
                                vec![(
                                    "Content-Range".to_string(),
                                    format!("bytes 0-0/{}", chunk1.len()),
                                )],
                                vec![chunk1[0]],
                            )
                        } else {
                            chunk_non_range_get_requests.fetch_add(1, Ordering::SeqCst);
                            (400, Vec::new(), b"missing range".to_vec())
                        }
                    }
                    _ => (404, Vec::new(), b"not found".to_vec()),
                }
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            // Use concurrency=1 to ensure the first chunk disables HEAD before chunk 1 is processed.
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result?;
        assert_eq!(chunk_head_requests.load(Ordering::SeqCst), 1);
        assert_eq!(chunk_range_get_requests.load(Ordering::SeqCst), 2);
        assert_eq!(chunk_non_range_get_requests.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_disables_head_on_content_length_mismatch() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let chunk_head_requests = Arc::new(AtomicU64::new(0));
        let chunk_get_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let chunk_head_requests = Arc::clone(&chunk_head_requests);
            let chunk_get_requests = Arc::clone(&chunk_get_requests);
            Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
                "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                "/meta.json" => (404, Vec::new(), b"not found".to_vec()),
                "/chunks/00000000.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        // Deliberately incorrect Content-Length to force a fallback to GET.
                        (
                            200,
                            vec![("Content-Length".to_string(), "0".to_string())],
                            Vec::new(),
                        )
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk0.clone())
                    }
                }
                "/chunks/00000001.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (
                            200,
                            vec![("Content-Length".to_string(), chunk1.len().to_string())],
                            Vec::new(),
                        )
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk1.clone())
                    }
                }
                _ => (404, Vec::new(), b"not found".to_vec()),
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            // Use concurrency=1 so head_supported can be deterministically disabled after the
            // first chunk's HEAD mismatch.
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result?;
        assert_eq!(
            chunk_head_requests.load(Ordering::SeqCst),
            1,
            "expected HEAD optimization to be disabled after the first mismatch"
        );
        assert_eq!(
            chunk_get_requests.load(Ordering::SeqCst),
            2,
            "expected verifier to fall back to GET for both chunks after disabling HEAD"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_disables_head_on_forbidden() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let chunk_head_requests = Arc::new(AtomicU64::new(0));
        let chunk_get_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let chunk_head_requests = Arc::clone(&chunk_head_requests);
            let chunk_get_requests = Arc::clone(&chunk_get_requests);
            Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
                "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                "/meta.json" => (404, Vec::new(), b"not found".to_vec()),
                // Some signed URL schemes are method-specific; HEAD may be rejected even when GET is allowed.
                "/chunks/00000000.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (403, Vec::new(), Vec::new())
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk0.clone())
                    }
                }
                "/chunks/00000001.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (403, Vec::new(), Vec::new())
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk1.clone())
                    }
                }
                _ => (404, Vec::new(), b"not found".to_vec()),
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            // Keep deterministic ordering so only the first chunk triggers the HEAD failure.
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result?;
        assert_eq!(
            chunk_head_requests.load(Ordering::SeqCst),
            1,
            "expected HEAD optimization to be disabled after the first 403"
        );
        assert_eq!(
            chunk_get_requests.load(Ordering::SeqCst),
            2,
            "expected verifier to fall back to GET for all chunks after disabling HEAD"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_disables_head_on_unauthorized() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let chunk_head_requests = Arc::new(AtomicU64::new(0));
        let chunk_get_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let chunk_head_requests = Arc::clone(&chunk_head_requests);
            let chunk_get_requests = Arc::clone(&chunk_get_requests);
            Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
                "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                "/meta.json" => (404, Vec::new(), b"not found".to_vec()),
                // Some signed URL schemes are method-specific; HEAD may be rejected even when GET is allowed.
                "/chunks/00000000.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (401, Vec::new(), Vec::new())
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk0.clone())
                    }
                }
                "/chunks/00000001.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (401, Vec::new(), Vec::new())
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk1.clone())
                    }
                }
                _ => (404, Vec::new(), b"not found".to_vec()),
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            // Keep deterministic ordering so only the first chunk triggers the HEAD failure.
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result?;
        assert_eq!(
            chunk_head_requests.load(Ordering::SeqCst),
            1,
            "expected HEAD optimization to be disabled after the first 401"
        );
        assert_eq!(
            chunk_get_requests.load(Ordering::SeqCst),
            2,
            "expected verifier to fall back to GET for all chunks after disabling HEAD"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_disables_head_on_not_implemented() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let chunk_head_requests = Arc::new(AtomicU64::new(0));
        let chunk_get_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let chunk_head_requests = Arc::clone(&chunk_head_requests);
            let chunk_get_requests = Arc::clone(&chunk_get_requests);
            Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
                "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                "/meta.json" => (404, Vec::new(), b"not found".to_vec()),
                "/chunks/00000000.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (501, Vec::new(), Vec::new())
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk0.clone())
                    }
                }
                "/chunks/00000001.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (501, Vec::new(), Vec::new())
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk1.clone())
                    }
                }
                _ => (404, Vec::new(), b"not found".to_vec()),
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            // Keep deterministic ordering so only the first chunk triggers the HEAD failure.
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result?;
        assert_eq!(
            chunk_head_requests.load(Ordering::SeqCst),
            1,
            "expected HEAD optimization to be disabled after the first 501"
        );
        assert_eq!(
            chunk_get_requests.load(Ordering::SeqCst),
            2,
            "expected verifier to fall back to GET for all chunks after disabling HEAD"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_disables_head_on_not_found() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let chunk_head_requests = Arc::new(AtomicU64::new(0));
        let chunk_get_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let chunk_head_requests = Arc::clone(&chunk_head_requests);
            let chunk_get_requests = Arc::clone(&chunk_get_requests);
            Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
                "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                "/meta.json" => (404, Vec::new(), b"not found".to_vec()),
                // Some servers return 404 for HEAD even though GET works (unsupported method / routing).
                "/chunks/00000000.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (404, Vec::new(), Vec::new())
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk0.clone())
                    }
                }
                "/chunks/00000001.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (404, Vec::new(), Vec::new())
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk1.clone())
                    }
                }
                _ => (404, Vec::new(), b"not found".to_vec()),
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            // Keep deterministic ordering so only the first chunk triggers the HEAD failure.
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result?;
        assert_eq!(
            chunk_head_requests.load(Ordering::SeqCst),
            1,
            "expected HEAD optimization to be disabled after the first 404"
        );
        assert_eq!(
            chunk_get_requests.load(Ordering::SeqCst),
            2,
            "expected verifier to fall back to GET for all chunks after disabling HEAD"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_falls_back_when_head_has_unexpected_content_encoding() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let chunk_head_requests = Arc::new(AtomicU64::new(0));
        let chunk_get_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let chunk_head_requests = Arc::clone(&chunk_head_requests);
            let chunk_get_requests = Arc::clone(&chunk_get_requests);
            Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
                "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                "/meta.json" => (404, Vec::new(), b"not found".to_vec()),
                "/chunks/00000000.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        // Deliberately incorrect header to ensure we fall back to GET instead of
                        // failing or trusting HEAD.
                        (
                            200,
                            vec![
                                ("Content-Encoding".to_string(), "gzip".to_string()),
                                ("Content-Length".to_string(), chunk0.len().to_string()),
                            ],
                            Vec::new(),
                        )
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk0.clone())
                    }
                }
                "/chunks/00000001.bin" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                        (
                            200,
                            vec![("Content-Length".to_string(), chunk1.len().to_string())],
                            Vec::new(),
                        )
                    } else {
                        chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk1.clone())
                    }
                }
                _ => (404, Vec::new(), b"not found".to_vec()),
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            // Keep deterministic ordering so only the first chunk triggers the HEAD mismatch.
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result?;
        assert_eq!(
            chunk_head_requests.load(Ordering::SeqCst),
            1,
            "expected HEAD optimization to be disabled after the first unexpected Content-Encoding"
        );
        assert_eq!(
            chunk_get_requests.load(Ordering::SeqCst),
            2,
            "expected verifier to fall back to GET for all chunks after disabling HEAD"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_manifest_url_without_chunks_list_detects_size_mismatch() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; (chunk_size as usize) - 1]; // wrong length
        let chunk1 = vec![b'b'; 512];
        let total_size = chunk_size + (chunk1.len() as u64);

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
            "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
            "/chunks/00000000.bin" => (200, Vec::new(), chunk0.clone()),
            "/chunks/00000001.bin" => (200, Vec::new(), chunk1.clone()),
            _ => (404, Vec::new(), b"not found".to_vec()),
        });

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        let err = result.expect_err("expected verify failure");
        let msg = err.to_string();
        assert!(
            msg.contains("size mismatch") && msg.contains("chunk 0"),
            "unexpected error message: {msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_manifest_url_fails_fast_without_fetching_other_chunks() -> Result<()> {
        use std::sync::Mutex;

        let chunk_size: u64 = 1024;
        let chunk1 = vec![b'b'; 512];
        let total_size = chunk_size + (chunk1.len() as u64);

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let requests_for_responder = Arc::clone(&requests);

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| {
            requests_for_responder
                .lock()
                .expect("lock requests")
                .push(req.path.clone());

            match req.path.as_str() {
                "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                "/meta.json" => (404, Vec::new(), b"not found".to_vec()),
                // Make the first chunk missing so verification fails immediately.
                "/chunks/00000000.bin" => (404, Vec::new(), b"not found".to_vec()),
                // This chunk should never be fetched when fail-fast is working (concurrency=1).
                "/chunks/00000001.bin" => (200, Vec::new(), chunk1.clone()),
                _ => (404, Vec::new(), b"not found".to_vec()),
            }
        });

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        let err = result.expect_err("expected verify failure");
        assert!(
            err.to_string().contains("chunk 0"),
            "unexpected error message: {err}"
        );

        let requests = requests.lock().expect("lock requests");
        assert!(
            requests.iter().any(|p| p == "/chunks/00000000.bin"),
            "expected chunk 0 request; saw: {requests:?}"
        );
        assert!(
            !requests.iter().any(|p| p == "/chunks/00000001.bin"),
            "expected verifier to fail fast and not fetch chunk 1; saw: {requests:?}"
        );

        Ok(())
    }

    #[tokio::test]
    async fn verify_http_preserves_manifest_query_for_chunks() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let token = "token=abc";

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
            // The query must be present on both manifest and chunk requests.
            "/manifest.json?token=abc" => (200, Vec::new(), manifest_bytes.clone()),
            "/chunks/00000000.bin?token=abc" => (200, Vec::new(), chunk0.clone()),
            "/chunks/00000001.bin?token=abc" => (200, Vec::new(), chunk1.clone()),
            // If the query is missing, make it a hard failure so the test would fail without
            // query preservation logic.
            "/chunks/00000000.bin" | "/chunks/00000001.bin" => {
                (401, Vec::new(), b"missing token".to_vec())
            }
            _ => (404, Vec::new(), b"not found".to_vec()),
        });

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json?{token}")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_preserves_manifest_query_for_head_chunks() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let token = "token=abc";

        let head_requests = Arc::new(AtomicU64::new(0));
        let get_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let head_requests = Arc::clone(&head_requests);
            let get_requests = Arc::clone(&get_requests);
            Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
                "/manifest.json?token=abc" => (200, Vec::new(), manifest_bytes.clone()),
                // Fail hard if the verifier drops the query.
                "/manifest.json" => (401, Vec::new(), b"missing token".to_vec()),
                "/meta.json?token=abc" => (404, Vec::new(), b"not found".to_vec()),
                "/meta.json" => (401, Vec::new(), b"missing token".to_vec()),
                "/chunks/00000000.bin?token=abc" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        head_requests.fetch_add(1, Ordering::SeqCst);
                        (
                            200,
                            vec![("Content-Length".to_string(), chunk0.len().to_string())],
                            Vec::new(),
                        )
                    } else {
                        get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk0.clone())
                    }
                }
                "/chunks/00000001.bin?token=abc" => {
                    if req.method.eq_ignore_ascii_case("HEAD") {
                        head_requests.fetch_add(1, Ordering::SeqCst);
                        (
                            200,
                            vec![("Content-Length".to_string(), chunk1.len().to_string())],
                            Vec::new(),
                        )
                    } else {
                        get_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk1.clone())
                    }
                }
                "/chunks/00000000.bin" | "/chunks/00000001.bin" => {
                    (401, Vec::new(), b"missing token".to_vec())
                }
                _ => (404, Vec::new(), b"not found".to_vec()),
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json?{token}")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        assert_eq!(get_requests.load(Ordering::SeqCst), 0);
        assert_eq!(head_requests.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_preserves_manifest_query_for_meta_json() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let meta = Meta {
            created_at: Utc::now(),
            original_filename: "disk.img".to_string(),
            total_size,
            chunk_size,
            chunk_count: manifest.chunk_count,
            checksum_algorithm: "sha256".to_string(),
        };
        let meta_bytes = serde_json::to_vec_pretty(&meta).context("serialize meta")?;

        let token = "token=abc";

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
            "/manifest.json?token=abc" => (200, Vec::new(), manifest_bytes.clone()),
            "/meta.json?token=abc" => (200, Vec::new(), meta_bytes.clone()),
            "/chunks/00000000.bin?token=abc" => (200, Vec::new(), chunk0.clone()),
            "/chunks/00000001.bin?token=abc" => (200, Vec::new(), chunk1.clone()),
            // Fail hard if the verifier drops the query.
            "/meta.json" => (401, Vec::new(), b"missing token".to_vec()),
            _ => (404, Vec::new(), b"not found".to_vec()),
        });

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json?{token}")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_retries_on_transient_500() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let manifest_requests = Arc::new(AtomicU64::new(0));
        let chunk0_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let manifest_requests = Arc::clone(&manifest_requests);
            let chunk0_requests = Arc::clone(&chunk0_requests);
            Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
                "/manifest.json" => {
                    let n = manifest_requests.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        (500, Vec::new(), b"oops".to_vec())
                    } else {
                        (200, Vec::new(), manifest_bytes.clone())
                    }
                }
                "/chunks/00000000.bin" => {
                    let n = chunk0_requests.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        (500, Vec::new(), b"oops".to_vec())
                    } else {
                        (200, Vec::new(), chunk0.clone())
                    }
                }
                "/chunks/00000001.bin" => (200, Vec::new(), chunk1.clone()),
                _ => (404, Vec::new(), b"not found".to_vec()),
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let result = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            // Must be > 1 to allow a retry after the deliberate 500.
            retries: 2,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        result?;
        assert!(manifest_requests.load(Ordering::SeqCst) >= 2);
        assert!(chunk0_requests.load(Ordering::SeqCst) >= 2);
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_sends_custom_headers() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let unauthorized_requests = Arc::new(AtomicU64::new(0));
        let manifest_requests = Arc::new(AtomicU64::new(0));
        let chunk_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let unauthorized_requests = Arc::clone(&unauthorized_requests);
            let manifest_requests = Arc::clone(&manifest_requests);
            let chunk_requests = Arc::clone(&chunk_requests);
            Arc::new(move |req: TestHttpRequest| {
                let expected = "Bearer test";
                let auth_ok = req
                    .headers
                    .iter()
                    .any(|(k, v)| k.eq_ignore_ascii_case("authorization") && v == expected);
                if !auth_ok {
                    unauthorized_requests.fetch_add(1, Ordering::SeqCst);
                    return (401, Vec::new(), b"unauthorized".to_vec());
                }

                match req.path.as_str() {
                    "/manifest.json" => {
                        manifest_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), manifest_bytes.clone())
                    }
                    "/chunks/00000000.bin" => {
                        chunk_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk0.clone())
                    }
                    "/chunks/00000001.bin" => {
                        chunk_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), chunk1.clone())
                    }
                    _ => (404, Vec::new(), b"not found".to_vec()),
                }
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;
        let manifest_url = format!("{base_url}/manifest.json");

        // Without headers, the server should reject with 401.
        let err = verify(VerifyArgs {
            manifest_url: Some(manifest_url.clone()),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await
        .unwrap_err();
        assert!(
            error_chain_summary(&err).contains("HTTP 401"),
            "unexpected error chain: {}",
            error_chain_summary(&err)
        );
        assert_eq!(unauthorized_requests.load(Ordering::SeqCst), 1);
        assert_eq!(manifest_requests.load(Ordering::SeqCst), 0);
        assert_eq!(chunk_requests.load(Ordering::SeqCst), 0);

        // With headers, verify should succeed.
        verify(VerifyArgs {
            manifest_url: Some(manifest_url),
            manifest_file: None,
            header: vec!["Authorization: Bearer test".to_string()],
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        assert_eq!(unauthorized_requests.load(Ordering::SeqCst), 1);
        assert_eq!(manifest_requests.load(Ordering::SeqCst), 1);
        assert_eq!(chunk_requests.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_sends_custom_headers_on_head_chunks() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let manifest = ManifestV1 {
            schema: MANIFEST_SCHEMA.to_string(),
            image_id: "demo".to_string(),
            version: "v1".to_string(),
            mime_type: CHUNK_MIME_TYPE.to_string(),
            total_size,
            chunk_size,
            chunk_count: chunk_count(total_size, chunk_size),
            chunk_index_width: CHUNK_INDEX_WIDTH as u32,
            chunks: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let unauthorized_requests = Arc::new(AtomicU64::new(0));
        let manifest_requests = Arc::new(AtomicU64::new(0));
        let chunk_head_requests = Arc::new(AtomicU64::new(0));
        let chunk_get_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let unauthorized_requests = Arc::clone(&unauthorized_requests);
            let manifest_requests = Arc::clone(&manifest_requests);
            let chunk_head_requests = Arc::clone(&chunk_head_requests);
            let chunk_get_requests = Arc::clone(&chunk_get_requests);
            Arc::new(move |req: TestHttpRequest| {
                let expected = "Bearer test";
                let auth_ok = req
                    .headers
                    .iter()
                    .any(|(k, v)| k.eq_ignore_ascii_case("authorization") && v == expected);
                if !auth_ok {
                    unauthorized_requests.fetch_add(1, Ordering::SeqCst);
                    return (401, Vec::new(), b"unauthorized".to_vec());
                }

                match req.path.as_str() {
                    "/manifest.json" => {
                        manifest_requests.fetch_add(1, Ordering::SeqCst);
                        (200, Vec::new(), manifest_bytes.clone())
                    }
                    "/meta.json" => (404, Vec::new(), b"not found".to_vec()),
                    "/chunks/00000000.bin" => {
                        if req.method.eq_ignore_ascii_case("HEAD") {
                            chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                            (
                                200,
                                vec![("Content-Length".to_string(), chunk0.len().to_string())],
                                Vec::new(),
                            )
                        } else {
                            chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                            (200, Vec::new(), chunk0.clone())
                        }
                    }
                    "/chunks/00000001.bin" => {
                        if req.method.eq_ignore_ascii_case("HEAD") {
                            chunk_head_requests.fetch_add(1, Ordering::SeqCst);
                            (
                                200,
                                vec![("Content-Length".to_string(), chunk1.len().to_string())],
                                Vec::new(),
                            )
                        } else {
                            chunk_get_requests.fetch_add(1, Ordering::SeqCst);
                            (200, Vec::new(), chunk1.clone())
                        }
                    }
                    _ => (404, Vec::new(), b"not found".to_vec()),
                }
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;
        let manifest_url = format!("{base_url}/manifest.json");

        // Without headers, the server should reject with 401.
        let err = verify(VerifyArgs {
            manifest_url: Some(manifest_url.clone()),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await
        .unwrap_err();
        assert!(
            error_chain_summary(&err).contains("HTTP 401"),
            "unexpected error chain: {}",
            error_chain_summary(&err)
        );
        assert_eq!(unauthorized_requests.load(Ordering::SeqCst), 1);
        assert_eq!(manifest_requests.load(Ordering::SeqCst), 0);
        assert_eq!(chunk_head_requests.load(Ordering::SeqCst), 0);
        assert_eq!(chunk_get_requests.load(Ordering::SeqCst), 0);

        // With headers, verify should succeed and use HEAD for chunk size validation.
        verify(VerifyArgs {
            manifest_url: Some(manifest_url),
            manifest_file: None,
            header: vec!["Authorization: Bearer test".to_string()],
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        assert_eq!(unauthorized_requests.load(Ordering::SeqCst), 1);
        assert_eq!(manifest_requests.load(Ordering::SeqCst), 1);
        assert_eq!(chunk_head_requests.load(Ordering::SeqCst), 2);
        assert_eq!(chunk_get_requests.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_sends_browser_like_accept_encoding() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let checked = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let checked = Arc::clone(&checked);
            Arc::new(move |req: TestHttpRequest| {
                let encoding = req
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"))
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("");
                if encoding != BROWSER_ACCEPT_ENCODING {
                    return (
                        400,
                        Vec::new(),
                        format!("unexpected accept-encoding: {encoding}").into_bytes(),
                    );
                }

                checked.fetch_add(1, Ordering::SeqCst);
                match req.path.as_str() {
                    "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                    "/chunks/00000000.bin" => (200, Vec::new(), chunk0.clone()),
                    "/chunks/00000001.bin" => (200, Vec::new(), chunk1.clone()),
                    _ => (404, Vec::new(), b"not found".to_vec()),
                }
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        // At minimum: manifest + 2 chunks. (Some optional requests like meta.json may also occur.)
        assert!(checked.load(Ordering::SeqCst) >= 3);
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_overrides_user_accept_encoding_header() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let checked = Arc::new(AtomicU64::new(0));
        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let checked = Arc::clone(&checked);
            Arc::new(move |req: TestHttpRequest| {
                let encoding = req
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"))
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("");
                if encoding != BROWSER_ACCEPT_ENCODING {
                    return (
                        400,
                        Vec::new(),
                        format!("unexpected accept-encoding: {encoding}").into_bytes(),
                    );
                }

                checked.fetch_add(1, Ordering::SeqCst);
                match req.path.as_str() {
                    "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                    "/chunks/00000000.bin" => (200, Vec::new(), chunk0.clone()),
                    "/chunks/00000001.bin" => (200, Vec::new(), chunk1.clone()),
                    _ => (404, Vec::new(), b"not found".to_vec()),
                }
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            // User tries to override accept-encoding; tool should force a browser-like value.
            header: vec!["Accept-Encoding: gzip".to_string()],
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 2,
            retries: 1,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await?;

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        assert!(checked.load(Ordering::SeqCst) >= 3);
        Ok(())
    }

    #[tokio::test]
    async fn verify_http_rejects_non_identity_content_encoding() -> Result<()> {
        let chunk_size: u64 = 1024;
        let chunk0 = vec![b'a'; chunk_size as usize];
        let chunk1 = vec![b'b'; 512];
        let total_size = (chunk0.len() + chunk1.len()) as u64;

        let sha256_by_index = vec![Some(sha256_hex(&chunk0)), Some(sha256_hex(&chunk1))];
        let manifest = build_manifest_v1(
            total_size,
            chunk_size,
            "demo",
            "v1",
            ChecksumAlgorithm::Sha256,
            &sha256_by_index,
        )?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;

        let chunk0_requests = Arc::new(AtomicU64::new(0));

        let responder: Arc<
            dyn Fn(TestHttpRequest) -> (u16, Vec<(String, String)>, Vec<u8>)
                + Send
                + Sync
                + 'static,
        > = {
            let chunk0_requests = Arc::clone(&chunk0_requests);
            Arc::new(move |req: TestHttpRequest| match req.path.as_str() {
                "/manifest.json" => (200, Vec::new(), manifest_bytes.clone()),
                "/chunks/00000000.bin" => {
                    chunk0_requests.fetch_add(1, Ordering::SeqCst);
                    (
                        200,
                        vec![("Content-Encoding".to_string(), "gzip".to_string())],
                        chunk0.clone(),
                    )
                }
                "/chunks/00000001.bin" => (200, Vec::new(), chunk1.clone()),
                _ => (404, Vec::new(), b"not found".to_vec()),
            })
        };

        let (base_url, shutdown_tx, server_handle) = start_test_http_server(responder).await?;

        let err = verify(VerifyArgs {
            manifest_url: Some(format!("{base_url}/manifest.json")),
            manifest_file: None,
            header: Vec::new(),
            bucket: None,
            prefix: None,
            manifest_key: None,
            image_id: None,
            image_version: None,
            endpoint: None,
            force_path_style: false,
            region: "us-east-1".to_string(),
            concurrency: 1,
            retries: 3,
            max_chunks: MAX_CHUNKS,
            chunk_sample: None,
            chunk_sample_seed: None,
        })
        .await
        .unwrap_err();

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        let msg = err.to_string();
        assert!(
            msg.contains("unexpected Content-Encoding") && msg.contains("chunk 0"),
            "unexpected error message: {msg}"
        );
        assert_eq!(chunk0_requests.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn check_chunk_bytes_validates_size_and_sha256() {
        let bytes = b"hello";
        let expected_sha = sha256_hex(bytes);
        assert_eq!(check_chunk_bytes(bytes, 5, Some(&expected_sha)), Ok(()));
        assert_eq!(
            check_chunk_bytes(bytes, 6, Some(&expected_sha)),
            Err(ChunkCheckError::SizeMismatch {
                expected: 6,
                actual: 5
            })
        );
        assert!(matches!(
            check_chunk_bytes(bytes, 5, Some("deadbeef")),
            Err(ChunkCheckError::Sha256Mismatch { .. })
        ));
    }

    #[test]
    fn cli_accepts_aerospar_format_alias() {
        let cli = Cli::parse_from([
            "aero-image-chunker",
            "publish",
            "--file",
            "disk.img",
            "--format",
            "aerospar",
            "--bucket",
            "bucket",
            "--prefix",
            "images/win7/sha256-abc/",
        ]);
        let Commands::Publish(args) = cli.command else {
            panic!("expected publish subcommand");
        };
        assert!(matches!(args.format, InputFormat::AeroSparse));
    }
}
