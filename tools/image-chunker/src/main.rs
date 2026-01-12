use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
use serde::Serialize;
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
    /// Chunk a raw disk image and publish it to an S3-compatible object store.
    Publish(PublishArgs),
}

#[derive(Debug, Parser)]
struct PublishArgs {
    /// Path to a raw disk image file.
    #[arg(long)]
    file: PathBuf,

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
    /// content before uploading (this requires reading the input file twice: hash, then upload).
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

#[derive(Debug, Serialize)]
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
    chunks: Vec<ManifestChunkV1>,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
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
    }
}

async fn publish(args: PublishArgs) -> Result<()> {
    validate_args(&args)?;

    let prefix = normalize_prefix(&args.prefix);
    let file_metadata = tokio::fs::metadata(&args.file)
        .await
        .with_context(|| format!("stat {}", args.file.display()))?;
    let total_size = file_metadata.len();
    let chunk_count = chunk_count(total_size, args.chunk_size);
    if chunk_count > MAX_CHUNKS {
        bail!(
            "image requires {chunk_count} chunks which exceeds the current limit of {MAX_CHUNKS} (chunk size too small?)"
        );
    }

    let computed_version = match args.compute_version {
        ComputeVersion::None => None,
        ComputeVersion::Sha256 => {
            eprintln!("Computing full-image SHA-256 version from {}...", args.file.display());
            Some(compute_image_version_sha256(&args.file).await?)
        }
    };

    let destination = resolve_publish_destination(&args, &prefix, computed_version.as_deref())?;
    let image_id = destination.image_id.clone();
    let version = destination.version.clone();
    let version_prefix = destination.version_prefix.clone();
    let manifest_key = manifest_object_key(&version_prefix);

    let s3 = build_s3_client(&args).await?;

    eprintln!(
        "Publishing {}\n  imageId: {}\n  version: {}\n  total size: {} bytes\n  chunk size: {} bytes\n  chunk count: {}\n  destination: s3://{}/{}",
        args.file.display(),
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

    let (work_tx, work_rx) = async_channel::bounded::<ChunkJob>(args.concurrency * 2);
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

    let mut file = tokio::fs::File::open(&args.file)
        .await
        .with_context(|| format!("open {}", args.file.display()))?;

    for index in 0..chunk_count {
        let offset = index
            .checked_mul(args.chunk_size)
            .ok_or_else(|| anyhow!("chunk offset overflows u64"))?;
        let remaining = total_size.saturating_sub(offset);
        let expected = std::cmp::min(args.chunk_size, remaining);
        let expected_usize: usize = expected
            .try_into()
            .map_err(|_| anyhow!("chunk size {expected} does not fit into usize"))?;
        let mut buf = vec![0u8; expected_usize];
        file.read_exact(&mut buf)
            .await
            .with_context(|| format!("read chunk {index} at offset {offset}"))?;

        let bytes = Bytes::from(buf);
        work_tx
            .send(ChunkJob { index, bytes })
            .await
            .map_err(|err| anyhow!("internal worker channel closed unexpectedly: {err}"))?;
    }

    drop(work_tx);

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

fn validate_args(args: &PublishArgs) -> Result<()> {
    if args.chunk_size == 0 {
        bail!("--chunk-size must be > 0");
    }
    if args.chunk_size % 512 != 0 {
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

    let ends_with_version = segments
        .last()
        .is_some_and(|segment| *segment == version)
        && segments.len() >= 2;
    let ends_with_image_id = segments
        .last()
        .is_some_and(|segment| *segment == image_id);

    let (version_prefix, image_root_prefix) = if ends_with_version {
        let inferred_image_id = segments[segments.len() - 2];
        if inferred_image_id != image_id {
            bail!(
                "--prefix implies imageId '{inferred_image_id}', but resolved imageId is '{image_id}'. Update --prefix or --image-id."
            );
        }
        (normalized_prefix.to_string(), parent_prefix(normalized_prefix)?)
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

fn chunk_object_key(index: u64) -> Result<String> {
    if index >= MAX_CHUNKS {
        bail!(
            "chunk index {index} exceeds max supported index {}",
            MAX_CHUNKS - 1
        );
    }
    Ok(format!(
        "chunks/{index:0width$}.bin",
        width = CHUNK_INDEX_WIDTH
    ))
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
        chunks,
    })
}

async fn build_s3_client(args: &PublishArgs) -> Result<S3Client> {
    let region_provider =
        RegionProviderChain::default_provider().or_else(Region::new(args.region.clone()));
    let shared_config = aws_config::defaults(BehaviorVersion::latest())
        .region(region_provider)
        .load()
        .await;

    let mut s3_config_builder = aws_sdk_s3::config::Builder::from(&shared_config);
    if let Some(endpoint) = &args.endpoint {
        s3_config_builder = s3_config_builder.endpoint_url(endpoint);
    }
    if args.force_path_style {
        s3_config_builder = s3_config_builder.force_path_style(true);
    }

    Ok(S3Client::from_conf(s3_config_builder.build()))
}

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

async fn compute_image_version_sha256(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("open {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .await
            .with_context(|| format!("read {} while hashing", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(sha256_version_from_digest(hasher.finalize()))
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
        let Commands::Publish(args) = cli.command;
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
        let manifest = build_manifest_v1(
            10,
            4,
            "win7",
            "sha256-abc",
            ChecksumAlgorithm::None,
            &[],
        )?;
        assert_eq!(manifest.total_size, 10);
        assert_eq!(manifest.chunk_size, 4);
        assert_eq!(manifest.chunk_count, 3);
        assert_eq!(manifest.chunks.len(), 3);
        assert_eq!(manifest.chunks[0].size, 4);
        assert_eq!(manifest.chunks[1].size, 4);
        assert_eq!(manifest.chunks[2].size, 2);
        assert_eq!(manifest.chunks[0].sha256, None);
        Ok(())
    }
}
