use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "aero_iso_ls")]
#[command(about = "List file paths in a Joliet ISO image (used by tools/driver-iso/verify_iso.py)")]
struct Cli {
    /// Input .iso path.
    #[arg(long)]
    iso: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let iso_bytes = std::fs::read(&cli.iso)?;
    let entries = aero_packager::read_joliet_file_entries(&iso_bytes)?;

    for e in entries {
        // Print in a similar format to xorriso: absolute paths rooted at "/".
        let p = e.path.trim_start_matches('/');
        println!("/{p}");
    }

    Ok(())
}

