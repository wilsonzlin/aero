use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

const MANIFEST_SCHEMA: &str = "aero.chunked-disk-image.v1";
const CHUNK_MIME_TYPE: &str = "application/octet-stream";
const JSON_MIME_TYPE: &str = "application/json";
const LATEST_SCHEMA: &str = "aero.chunked-disk-image.latest.v1";
const DEFAULT_CACHE_CONTROL_CHUNKS: &str = "public, max-age=31536000, immutable, no-transform";
const DEFAULT_CACHE_CONTROL_MANIFEST: &str = "public, max-age=31536000, immutable";
const DEFAULT_CACHE_CONTROL_LATEST: &str = "public, max-age=60";
const CHUNK_CONTENT_ENCODING: &str = "identity";
const DEFAULT_CHUNK_SIZE_BYTES: u64 = 4 * 1024 * 1024;
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

    /// Chunk size in bytes.
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
#[command(group(
    clap::ArgGroup::new("location")
        .required(true)
        .args(["prefix", "manifest_key"])
        .multiple(false)
))]
struct VerifyArgs {
    /// Destination bucket name.
    #[arg(long)]
    bucket: String,

    /// Prefix of a versioned image (e.g. `images/<imageId>/<version>/`) or an image root
    /// (e.g. `images/<imageId>/`) when combined with `--image-version`.
    ///
    /// The tool will fetch `<prefix>/manifest.json` (versioned prefix) or
    /// `<prefix>/<imageVersion>/manifest.json` (image root + `--image-version`).
    ///
    /// If `<prefix>/manifest.json` is not found and `--image-version` is not provided, the tool
    /// will attempt to resolve `latest.json` under the given prefix and verify the referenced
    /// versioned manifest instead.
    #[arg(long)]
    prefix: Option<String>,

    /// Explicit object key of `manifest.json` to verify.
    ///
    /// Mutually exclusive with `--prefix`.
    #[arg(long)]
    manifest_key: Option<String>,

    /// Expected image identifier (validated against the manifest).
    #[arg(long)]
    image_id: Option<String>,

    /// Expected version identifier (validated against the manifest).
    #[arg(long)]
    image_version: Option<String>,

    /// Custom S3 endpoint URL (e.g. http://localhost:9000 for MinIO).
    #[arg(long)]
    endpoint: Option<String>,

    /// Use path-style addressing (required for some S3-compatible endpoints).
    #[arg(long, default_value_t = false)]
    force_path_style: bool,

    /// AWS region.
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
        default_value_t = MAX_CHUNKS,
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
    #[value(name = "aerosparse")]
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
    size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
}

#[derive(Debug, Serialize)]
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
    if !total_size.is_multiple_of(sector) {
        bail!("virtual disk size {total_size} is not a multiple of {sector} bytes");
    }
    let chunk_count = chunk_count(total_size, args.chunk_size);
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
    let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel::<ChunkResult>();

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

    reader_handle
        .await
        .map_err(|err| anyhow!("disk reader panicked: {err}"))??;

    for handle in workers {
        handle
            .await
            .map_err(|err| anyhow!("upload worker panicked: {err}"))??;
    }

    let mut sha256_by_index: Vec<Option<String>> =
        if matches!(args.checksum, ChecksumAlgorithm::Sha256) {
            vec![None; chunk_count as usize]
        } else {
            Vec::new()
        };

    while let Some(result) = result_rx.recv().await {
        if matches!(args.checksum, ChecksumAlgorithm::Sha256) {
            let idx: usize = result
                .index
                .try_into()
                .map_err(|_| anyhow!("chunk index {} does not fit into usize", result.index))?;
            sha256_by_index[idx] = result.sha256;
        }
    }

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

    let s3 = build_s3_client(
        args.endpoint.as_deref(),
        args.force_path_style,
        &args.region,
    )
    .await?;

    let mut manifest_key = resolve_verify_manifest_key(&args)?;
    eprintln!("Downloading s3://{}/{}...", args.bucket, manifest_key);

    // If the user provided `--prefix` without `--image-version`, the prefix may refer to either a
    // versioned image prefix (`.../<version>/`) or an image root (`.../<imageId>/`). If
    // `<prefix>/manifest.json` does not exist, fall back to resolving `latest.json` (if present).
    let mut latest_from_prefix: Option<(String, LatestV1)> = None;

    let manifest: ManifestV1 = match download_json_object_with_retry(
        &s3,
        &args.bucket,
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
                args.bucket, manifest_key, args.bucket, latest_key
            );
            let latest = download_json_object_optional_with_retry::<LatestV1>(
                &s3,
                &args.bucket,
                &latest_key,
                args.retries,
            )
            .await?
            .ok_or_else(|| {
                err.context(format!(
                    "manifest.json was not found at s3://{}/{}. If --prefix is an image root prefix, either pass --image-version, or publish a latest pointer at s3://{}/{}.",
                    args.bucket, manifest_key, args.bucket, latest_key
                ))
            })?;

            manifest_key = latest.manifest_key.clone();
            eprintln!(
                "Downloading s3://{}/{} (from latest.json)...",
                args.bucket, manifest_key
            );
            let manifest: ManifestV1 =
                download_json_object_with_retry(&s3, &args.bucket, &manifest_key, args.retries)
                    .await?;
            latest_from_prefix = Some((image_root_prefix, latest));
            manifest
        }
        Err(err) => return Err(err),
    };
    validate_manifest_v1(&manifest, args.max_chunks)?;
    let manifest = Arc::new(manifest);

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

    // Optional sanity check: if `latest.json` exists at the inferred image root, validate it.
    // Skip if we already validated `latest.json` as part of resolving `--prefix` above.
    if latest_from_prefix.is_none() {
        if let Ok(image_root_prefix) = parent_prefix(&version_prefix) {
            let latest_key = latest_object_key(&image_root_prefix);
            match download_json_object_optional_with_retry::<LatestV1>(
                &s3,
                &args.bucket,
                &latest_key,
                args.retries,
            )
            .await?
            {
                None => {
                    eprintln!(
                        "Note: s3://{}/{} not found; skipping latest pointer validation.",
                        args.bucket, latest_key
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
                        head_object_with_retry(
                            &s3,
                            &args.bucket,
                            &latest.manifest_key,
                            args.retries,
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "latest.json points at missing manifest s3://{}/{}",
                                args.bucket, latest.manifest_key
                            )
                        })?;
                    }
                }
            }
        }
    }

    verify_chunks(
        &s3,
        &args.bucket,
        &version_prefix,
        Arc::clone(&manifest),
        args.concurrency,
        args.retries,
        args.chunk_sample,
        args.chunk_sample_seed,
    )
    .await?;

    eprintln!("OK.");
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
    if !manifest.chunk_size.is_multiple_of(512) {
        bail!(
            "manifest chunkSize must be a multiple of 512 bytes, got {}",
            manifest.chunk_size
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
            if chunk.size != expected_size {
                bail!(
                    "manifest chunk[{idx_u64}].size mismatch: expected {expected_size}, got {}",
                    chunk.size
                );
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

    let chunks_verified = Arc::new(AtomicU64::new(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    let chunk_index_width: usize = manifest.chunk_index_width.try_into().map_err(|_| {
        anyhow!(
            "manifest chunkIndexWidth {} does not fit into usize",
            manifest.chunk_index_width
        )
    })?;

    #[derive(Debug, Clone)]
    struct VerifyChunkJob {
        index: u64,
    }

    let (work_tx, work_rx) = async_channel::bounded::<VerifyChunkJob>(concurrency * 2);
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
        let chunks_verified = Arc::clone(&chunks_verified);
        let cancelled = Arc::clone(&cancelled);
        workers.push(tokio::spawn(async move {
            while let Ok(job) = work_rx.recv().await {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                let key = format!(
                    "{version_prefix}{}",
                    chunk_object_key_with_width(job.index, chunk_index_width)?
                );
                let expected_size = expected_chunk_size(manifest.as_ref(), job.index)?;
                let expected_sha256 = expected_chunk_sha256(manifest.as_ref(), job.index)?;
                match verify_chunk_with_retry(
                    &s3,
                    &bucket,
                    &key,
                    expected_size,
                    expected_sha256,
                    retries,
                )
                .await
                {
                    Ok(()) => {
                        pb.inc(expected_size);
                        let done = chunks_verified.fetch_add(1, Ordering::SeqCst) + 1;
                        pb.set_message(format!("{done}/{total_chunks_to_verify} chunks"));
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

    let send_jobs = async {
        if let Some(indices) = indices {
            for index in indices {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                work_tx
                    .send(VerifyChunkJob { index })
                    .await
                    .map_err(|err| anyhow!("internal worker channel closed unexpectedly: {err}"))?;
            }
        } else {
            for index in 0..chunk_count {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                tokio::select! {
                    res = work_tx.send(VerifyChunkJob { index }) => {
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

    // If we stopped sending due to an error received via `err_rx`, abort workers immediately.
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

    // Wait for completion or a worker error.
    let worker_err = err_rx.recv().await;
    if let Some(err) = worker_err {
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

    pb.finish_with_message(format!(
        "{total_chunks_to_verify}/{total_chunks_to_verify} chunks"
    ));
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
        Ok(chunk.size)
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
    let resp = s3
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|err| {
            if is_no_such_key_error(&err) {
                anyhow!("object not found (404)")
            } else {
                anyhow!(err)
            }
        })
        .with_context(|| format!("GET s3://{bucket}/{key}"))?;

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
    }

    // If we don't have a checksum to verify, `Content-Length` already validated the size. Avoid
    // streaming the entire object body unnecessarily.
    if expected_sha256.is_none() && content_length.is_some() {
        return Ok(());
    }

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
                let aggregated = output
                    .body
                    .collect()
                    .await
                    .with_context(|| format!("read s3://{bucket}/{key}"))?;
                return Ok(aggregated.into_bytes());
            }
            Err(err) if is_no_such_key_error(&err) => {
                return Err(anyhow!("object not found (404) for s3://{bucket}/{key}"));
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
                let aggregated = output
                    .body
                    .collect()
                    .await
                    .with_context(|| format!("read s3://{bucket}/{key}"))?;
                return Ok(Some(aggregated.into_bytes()));
            }
            Err(err) if is_no_such_key_error(&err) => return Ok(None),
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

fn sdk_error_status_code<E>(err: &aws_sdk_s3::error::SdkError<E>) -> Option<u16> {
    use aws_sdk_s3::error::SdkError;

    match err {
        SdkError::ServiceError(service_err) => Some(service_err.raw().status().as_u16()),
        SdkError::ResponseError(resp_err) => Some(resp_err.raw().status().as_u16()),
        _ => None,
    }
}

fn validate_args(args: &PublishArgs) -> Result<()> {
    if args.chunk_size == 0 {
        bail!("--chunk-size must be > 0");
    }
    if !args.chunk_size.is_multiple_of(512) {
        bail!("--chunk-size must be a multiple of 512 bytes");
    }
    if args.concurrency == 0 {
        bail!("--concurrency must be > 0");
    }
    if args.retries == 0 {
        bail!("--retries must be > 0");
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

        chunks.push(ManifestChunkV1 { size, sha256 });
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
    result_tx: tokio::sync::mpsc::UnboundedSender<ChunkResult>,
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
            Some(CHUNK_CONTENT_ENCODING),
            retries,
        )
        .await?;

        pb.inc(size);
        let uploaded = chunks_uploaded.fetch_add(1, Ordering::SeqCst) + 1;
        pb.set_message(format!("{uploaded}/{chunk_count} chunks"));

        let _ = result_tx.send(ChunkResult {
            index: job.index,
            sha256,
        });
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
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
        None,
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

    #[test]
    fn default_cache_control_values_match_docs() {
        assert_eq!(
            DEFAULT_CACHE_CONTROL_CHUNKS,
            "public, max-age=31536000, immutable, no-transform"
        );
        assert_eq!(
            DEFAULT_CACHE_CONTROL_MANIFEST,
            "public, max-age=31536000, immutable"
        );
        assert_eq!(DEFAULT_CACHE_CONTROL_LATEST, "public, max-age=60");
    }

    #[test]
    fn default_chunk_size_is_4_mib() {
        assert_eq!(DEFAULT_CHUNK_SIZE_BYTES, 4 * 1024 * 1024);
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
        assert_eq!(chunks[0].size, 4);
        assert_eq!(chunks[1].size, 4);
        assert_eq!(chunks[2].size, 2);
        assert_eq!(chunks[0].sha256, None);
        Ok(())
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
}
