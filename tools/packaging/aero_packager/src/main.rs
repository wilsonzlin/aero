use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "aero_packager")]
#[command(about = "Build the distributable Aero Drivers / Guest Tools ISO + zip", long_about = None)]
struct Cli {
    /// Directory containing built driver artifacts. Must contain `x86/` and `amd64/` (or `x64/`).
    #[arg(long)]
    drivers_dir: PathBuf,

    /// Directory containing Guest Tools scripts (setup.cmd, uninstall.cmd, README.md, etc).
    ///
    /// Note: `certs/` may be empty/absent when `--signing-policy none` is used (WHQL/production-signed drivers).
    #[arg(long)]
    guest_tools_dir: PathBuf,

    /// Output directory (will be created if missing).
    #[arg(long)]
    out_dir: PathBuf,

    /// JSON spec describing drivers (required + optional) and expected hardware IDs.
    #[arg(long)]
    spec: PathBuf,

    /// Version string to embed in manifest.json.
    #[arg(long, default_value = "0.0.0")]
    version: String,

    /// Build identifier to embed in manifest.json (e.g. CI run number).
    #[arg(long, default_value = "local")]
    build_id: String,

    /// ISO volume identifier (up to 32 characters).
    #[arg(long, default_value = "AERO_GUEST_TOOLS")]
    volume_id: String,

    /// Driver signing / boot policy for the packaged media.
    ///
    /// - `testsigning`: prompt to enable Test Signing on Win7 x64 (default)
    /// - `nointegritychecks`: prompt to disable signature enforcement on Win7 x64
    /// - `none`: do not prompt or change boot policy (for WHQL/production-signed drivers)
    #[arg(
        long,
        env = "AERO_GUEST_TOOLS_SIGNING_POLICY",
        default_value = "testsigning"
    )]
    signing_policy: aero_packager::SigningPolicy,

    /// Override SOURCE_DATE_EPOCH (seconds since Unix epoch) for deterministic timestamps.
    #[arg(long)]
    source_date_epoch: Option<i64>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let source_date_epoch = cli
        .source_date_epoch
        .or_else(|| std::env::var("SOURCE_DATE_EPOCH").ok()?.parse().ok())
        .unwrap_or(0);

    let config = aero_packager::PackageConfig {
        drivers_dir: cli.drivers_dir,
        guest_tools_dir: cli.guest_tools_dir,
        out_dir: cli.out_dir,
        spec_path: cli.spec,
        version: cli.version,
        build_id: cli.build_id,
        volume_id: cli.volume_id,
        signing_policy: cli.signing_policy,
        source_date_epoch,
    };

    let outputs = aero_packager::package_guest_tools(&config)?;
    println!(
        "wrote:\n- {}\n- {}\n- {}",
        outputs.iso_path.display(),
        outputs.zip_path.display(),
        outputs.manifest_path.display()
    );
    Ok(())
}
