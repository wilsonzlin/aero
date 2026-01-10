use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};

use bcd_patch::{patch_bcd_store, patch_win7_tree, PatchOpts};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Debug, Clone, Args)]
struct PatchArgs {
    /// Enable/disable testsigning (default: on).
    #[arg(long, value_enum)]
    testsigning: Option<OnOff>,

    /// Enable/disable nointegritychecks (default: on).
    #[arg(long, value_enum)]
    nointegritychecks: Option<OnOff>,
}

impl PatchArgs {
    fn to_opts(&self) -> PatchOpts {
        PatchOpts {
            testsigning: matches!(self.testsigning, None | Some(OnOff::On)),
            nointegritychecks: matches!(self.nointegritychecks, None | Some(OnOff::On)),
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "bcd_patch")]
#[command(about = "Offline patching of Windows BCD stores (REGF hives)")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to the offline BCD store file (e.g. boot/BCD).
    #[arg(long)]
    store: Option<PathBuf>,

    #[command(flatten)]
    patch: PatchArgs,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Patch all relevant Windows 7 BCD stores in an extracted ISO/tree.
    Win7Tree {
        /// Root of the extracted Windows 7 install media or image tree.
        #[arg(long)]
        root: PathBuf,

        #[command(flatten)]
        patch: PatchArgs,

        /// Treat any missing store as an error.
        #[arg(long)]
        strict: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Win7Tree {
            root,
            patch,
            strict,
        }) => {
            let report = patch_win7_tree(&root, patch.to_opts(), strict)?;

            for missing in &report.missing {
                eprintln!("warning: missing BCD store: {missing}");
            }

            for entry in &report.patched {
                if entry.changed {
                    println!("patched: {}", entry.path.display());
                } else {
                    println!("unchanged: {}", entry.path.display());
                }
            }

            println!(
                "summary: patched {} store(s), missing {} store(s)",
                report.patched.len(),
                report.missing.len()
            );

            Ok(())
        }
        None => {
            let store = cli
                .store
                .context("--store is required (unless using the win7-tree subcommand)")?;
            patch_bcd_store(&store, cli.patch.to_opts())
        }
    }
}
