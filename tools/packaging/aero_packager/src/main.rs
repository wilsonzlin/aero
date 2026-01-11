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
    /// Note: `certs/` may be empty/absent when `--signing-policy production` or `--signing-policy none`
    /// is used (no custom certificate required).
    #[arg(long)]
    guest_tools_dir: PathBuf,

    /// Output directory (will be created if missing).
    #[arg(long)]
    out_dir: PathBuf,

    /// JSON spec describing drivers (required + optional) and expected hardware IDs.
    #[arg(long)]
    spec: PathBuf,

    /// Machine-readable device contract used to generate `config/devices.cmd`.
    ///
    /// If omitted, defaults to `../docs/windows-device-contract.json` relative to `--guest-tools-dir`.
    #[arg(long)]
    windows_device_contract: Option<PathBuf>,

    /// Version string to embed in manifest.json.
    #[arg(long, default_value = "0.0.0")]
    version: String,

    /// Build identifier to embed in manifest.json (e.g. CI run number).
    #[arg(long, default_value = "local")]
    build_id: String,

    /// ISO volume identifier (up to 32 characters).
    #[arg(long, default_value = "AERO_GUEST_TOOLS")]
    volume_id: String,

    /// Signing expectations for the packaged drivers.
    ///
    /// - test: requires shipping at least one certificate in guest-tools/certs/ and will prompt
    ///   setup.cmd to enable Test Signing on Windows 7 x64.
    /// - production: drivers are production/WHQL-signed; no custom certificate is expected.
    /// - none: no signing expectations (development use).
    ///
    /// Legacy aliases accepted: `testsigning`, `test-signing`.
    #[arg(long, value_enum, env = "AERO_GUEST_TOOLS_SIGNING_POLICY", default_value = "test")]
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

    let windows_device_contract_path = match cli.windows_device_contract {
        Some(p) => p,
        None => {
            let base = cli
                .guest_tools_dir
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            base.join("docs").join("windows-device-contract.json")
        }
    };

    let config = aero_packager::PackageConfig {
        drivers_dir: cli.drivers_dir,
        guest_tools_dir: cli.guest_tools_dir,
        windows_device_contract_path,
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
