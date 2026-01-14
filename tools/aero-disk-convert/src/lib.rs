#![cfg(not(target_arch = "wasm32"))]

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use aero_storage::{
    AeroSparseConfig, AeroSparseDisk, DiskFormat, DiskImage, FileBackend, RawDisk, VirtualDisk,
};
use anyhow::{bail, Context};
use clap::ValueEnum;
use indicatif::{ProgressBar, ProgressStyle};

pub const DEFAULT_AEROSPARSE_BLOCK_SIZE_BYTES: u32 = 1024 * 1024; // 1 MiB
const COPY_CHUNK_BYTES: usize = 1024 * 1024; // 1 MiB

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum OutputFormat {
    Raw,
    #[value(alias = "aero-sparse")]
    AeroSparse,
}

impl OutputFormat {
    pub fn as_disk_format(self) -> DiskFormat {
        match self {
            Self::Raw => DiskFormat::Raw,
            Self::AeroSparse => DiskFormat::AeroSparse,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub input: PathBuf,
    pub output: PathBuf,
    pub output_format: OutputFormat,
    pub block_size_bytes: u32,
    pub progress: bool,
    pub force: bool,
}

pub fn convert(opts: ConvertOptions) -> anyhow::Result<()> {
    let input_canon = std::fs::canonicalize(&opts.input)
        .with_context(|| format!("failed to canonicalize input path {}", opts.input.display()))?;
    let output_canon = canonicalize_output_path(&opts.output)
        .with_context(|| format!("failed to canonicalize output path {}", opts.output.display()))?;
    if input_canon == output_canon {
        bail!("refusing to overwrite input file in-place");
    }

    let mut input = open_input_disk(&opts.input)?;
    let capacity = input.capacity_bytes();

    match opts.output_format {
        OutputFormat::Raw => {
            let backend = create_output_backend(&opts.output, opts.force)?;
            let mut out = RawDisk::create(backend, capacity)?;
            let pb = maybe_progress_bar(opts.progress, capacity, "raw")?;
            copy_all(&mut input, &mut out, capacity, COPY_CHUNK_BYTES, pb.as_ref())?;
            out.flush()?;
            if let Some(pb) = pb {
                pb.finish_and_clear();
            }
        }
        OutputFormat::AeroSparse => {
            let backend = create_output_backend(&opts.output, opts.force)?;
            let mut out = AeroSparseDisk::create(
                backend,
                AeroSparseConfig {
                    disk_size_bytes: capacity,
                    block_size_bytes: opts.block_size_bytes,
                },
            )?;
            let pb = maybe_progress_bar(opts.progress, capacity, "aerosparse")?;
            copy_skip_zero_blocks(
                &mut input,
                &mut out,
                capacity,
                opts.block_size_bytes as usize,
                pb.as_ref(),
            )?;
            out.flush()?;
            if let Some(pb) = pb {
                pb.finish_and_clear();
            }
        }
    }

    Ok(())
}

fn open_input_disk(path: &Path) -> anyhow::Result<DiskImage<FileBackend>> {
    let backend = FileBackend::open_read_only(path)
        .with_context(|| format!("failed to open input file {}", path.display()))?;
    Ok(DiskImage::open_auto(backend)?)
}

fn create_output_backend(path: &Path, force: bool) -> anyhow::Result<FileBackend> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create output directory {}",
                    parent.display()
                )
            })?;
        }
    }

    let mut opts = OpenOptions::new();
    opts.read(true).write(true);
    if force {
        opts.create(true).truncate(true);
    } else {
        opts.create_new(true);
    }

    let file = opts
        .open(path)
        .with_context(|| format!("failed to create output file {}", path.display()))?;
    Ok(FileBackend::from_file_with_path(file, path))
}

fn canonicalize_output_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        return std::fs::canonicalize(path);
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let parent = match std::fs::canonicalize(parent) {
        Ok(p) => p,
        // If the output directory doesn't exist yet, fall back to an absolute-ish path
        // based on the current directory.
        Err(_) => return Ok(std::env::current_dir()?.join(path)),
    };
    Ok(parent.join(
        path.file_name()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad filename"))?,
    ))
}

fn maybe_progress_bar(
    enabled: bool,
    total_bytes: u64,
    label: &'static str,
) -> anyhow::Result<Option<ProgressBar>> {
    if !enabled {
        return Ok(None);
    }
    let pb = ProgressBar::new(total_bytes);
    pb.set_message(label);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} {msg} [{elapsed_precise}] {bytes}/{total_bytes} ({percent}%) {wide_bar} {bytes_per_sec}",
        )?
        .progress_chars("=> "),
    );
    Ok(Some(pb))
}

fn copy_all(
    input: &mut dyn VirtualDisk,
    output: &mut dyn VirtualDisk,
    capacity: u64,
    chunk_bytes: usize,
    pb: Option<&ProgressBar>,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; chunk_bytes];
    let mut offset = 0u64;
    while offset < capacity {
        let remaining = capacity - offset;
        let read_len = (remaining as usize).min(buf.len());
        input.read_at(offset, &mut buf[..read_len])?;
        output.write_at(offset, &buf[..read_len])?;
        offset += read_len as u64;
        if let Some(pb) = pb {
            pb.inc(read_len as u64);
        }
    }
    Ok(())
}

fn copy_skip_zero_blocks(
    input: &mut dyn VirtualDisk,
    output: &mut dyn VirtualDisk,
    capacity: u64,
    block_bytes: usize,
    pb: Option<&ProgressBar>,
) -> anyhow::Result<()> {
    if block_bytes == 0 {
        bail!("block_size_bytes must be non-zero");
    }

    let mut buf = vec![0u8; block_bytes];
    let mut offset = 0u64;
    while offset < capacity {
        let remaining = capacity - offset;
        let read_len = (remaining as usize).min(buf.len());
        let chunk = &mut buf[..read_len];
        input.read_at(offset, chunk)?;
        if chunk.iter().any(|b| *b != 0) {
            output.write_at(offset, chunk)?;
        }
        offset += read_len as u64;
        if let Some(pb) = pb {
            pb.inc(read_len as u64);
        }
    }
    Ok(())
}
