use std::path::PathBuf;
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

    /// Key prefix to upload under (e.g. images/<imageId>/<version>/).
    #[arg(long)]
    prefix: String,

    /// Image identifier written into the manifest (recommended stable id, e.g. `win7-sp1-x64`).
    ///
    /// If omitted, it is inferred from `--prefix` by taking the second-to-last non-empty path
    /// segment.
    #[arg(long)]
    image_id: Option<String>,

    /// Version identifier written into the manifest (recommended: content hash, e.g. `sha256-...`).
    ///
    /// If omitted:
    /// - with `--compute-version none` (default): inferred from `--prefix` by taking the last
    ///   non-empty path segment.
    /// - with `--compute-version sha256`: computed as `sha256-<digest>` over the entire input
    ///   image stream.
    #[arg(long)]
    image_version: Option<String>,

    /// Compute a full-image version identifier while streaming the input image.
    ///
    /// When set to `sha256`, the tool computes `sha256-<digest>` over the entire input stream
    /// without performing a second read pass.
    ///
    /// If `--image-version` is omitted, the computed hash becomes the manifest `version`.
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Publish(args) => publish(args).await,
    }
}

async fn publish(args: PublishArgs) -> Result<()> {
    validate_args(&args)?;

    let prefix = normalize_prefix(&args.prefix);
    let (image_id, version) = resolve_image_id_and_version(&args, &prefix)?;
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

    let s3 = build_s3_client(&args).await?;

    let version_display = version.as_deref().unwrap_or("<computed (sha256)>");
    eprintln!(
        "Publishing {}\n  imageId: {}\n  version: {}\n  total size: {} bytes\n  chunk size: {} bytes\n  chunk count: {}\n  destination: s3://{}/{}",
        args.file.display(),
        image_id,
        version_display,
        total_size,
        args.chunk_size,
        chunk_count,
        args.bucket,
        prefix
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
        let prefix = prefix.clone();
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

    let mut full_image_hasher = match args.compute_version {
        ComputeVersion::None => None,
        ComputeVersion::Sha256 => Some(Sha256::new()),
    };

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

        if let Some(hasher) = full_image_hasher.as_mut() {
            hasher.update(&buf);
        }

        let bytes = Bytes::from(buf);
        work_tx
            .send(ChunkJob { index, bytes })
            .await
            .map_err(|err| anyhow!("internal worker channel closed unexpectedly: {err}"))?;
    }

    drop(work_tx);

    let computed_version = full_image_hasher
        .take()
        .map(|hasher| sha256_version_from_digest(hasher.finalize()));

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

    if let Some(computed_version) = &computed_version {
        if args.image_version.is_some() {
            eprintln!(
                "Computed full-image version hash: {computed_version} (not used; --image-version was provided)"
            );
        } else {
            eprintln!("Computed full-image version: {computed_version}");
        }
    }

    let version = version
        .or(computed_version)
        .expect("image version must be resolved at this point");

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
        &format!("{prefix}manifest.json"),
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
            &format!("{prefix}meta.json"),
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
            manifest_key: format!("{prefix}manifest.json"),
        };
        upload_json_object(
            &s3,
            &args.bucket,
            &format!("images/{image_id}/latest.json"),
            &latest,
            DEFAULT_CACHE_CONTROL_LATEST,
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
    (total_size + chunk_size - 1) / chunk_size
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

fn resolve_image_id_and_version(args: &PublishArgs, prefix: &str) -> Result<(String, Option<String>)> {
    let inferred = infer_image_id_and_version(prefix);
    let image_id = args
        .image_id
        .clone()
        .or_else(|| inferred.as_ref().map(|(image_id, _)| image_id.clone()))
        .ok_or_else(|| {
            anyhow!("--image-id is required when it cannot be inferred from --prefix")
        })?;
    let version = if let Some(version) = &args.image_version {
        Some(version.clone())
    } else if matches!(args.compute_version, ComputeVersion::Sha256) {
        None
    } else {
        Some(
            inferred
                .as_ref()
                .map(|(_, version)| version.clone())
                .ok_or_else(|| {
                    anyhow!("--image-version is required when it cannot be inferred from --prefix")
                })?,
        )
    };
    Ok((image_id, version))
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
    fn resolve_image_id_and_version_infers_from_prefix() -> Result<()> {
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
        let (image_id, version) = resolve_image_id_and_version(&args, &prefix)?;
        assert_eq!(image_id, "win7");
        assert_eq!(version.as_deref(), Some("sha256-abc"));
        Ok(())
    }

    #[test]
    fn resolve_image_id_and_version_defers_when_compute_version_enabled() -> Result<()> {
        let args = PublishArgs {
            file: PathBuf::from("disk.img"),
            bucket: "bucket".to_string(),
            prefix: "images/win7/sha256-abc/".to_string(),
            image_id: None,
            image_version: None,
            compute_version: ComputeVersion::Sha256,
            publish_latest: false,
            cache_control_chunks: DEFAULT_CACHE_CONTROL_CHUNKS.to_string(),
            cache_control_manifest: DEFAULT_CACHE_CONTROL_MANIFEST.to_string(),
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
        let (image_id, version) = resolve_image_id_and_version(&args, &prefix)?;
        assert_eq!(image_id, "win7");
        assert_eq!(version, None);
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
