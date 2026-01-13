use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "aero_iso")]
#[command(
    about = "Build a deterministic ISO9660 + Joliet image from a directory tree",
    long_about = None
)]
struct Cli {
    /// Input directory containing the files to package.
    #[arg(long)]
    in_dir: PathBuf,

    /// Output .iso path (will be overwritten if it already exists).
    #[arg(long)]
    out_iso: PathBuf,

    /// ISO volume identifier (up to 32 characters; will be normalized to ISO9660 constraints).
    #[arg(long)]
    volume_id: String,

    /// Seconds since Unix epoch used for timestamps inside the ISO.
    ///
    /// Defaults to `SOURCE_DATE_EPOCH` if set, otherwise 0.
    #[arg(long, env = "SOURCE_DATE_EPOCH", default_value_t = 0)]
    source_date_epoch: i64,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if let Some(parent) = cli.out_iso.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    aero_packager::write_iso9660_joliet_from_dir(
        &cli.in_dir,
        &cli.out_iso,
        &cli.volume_id,
        cli.source_date_epoch,
    )?;

    Ok(())
}

