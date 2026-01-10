use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ToggleArg {
    On,
    Off,
}

impl From<ToggleArg> for bcd_patch::Toggle {
    fn from(value: ToggleArg) -> Self {
        match value {
            ToggleArg::On => bcd_patch::Toggle::On,
            ToggleArg::Off => bcd_patch::Toggle::Off,
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "bcd_patch")]
#[command(about = "Offline BCD patcher utilities")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Patch all relevant Windows 7 BCD stores in an extracted ISO/tree.
    Win7Tree {
        /// Root of the extracted Windows 7 install media or image tree.
        #[arg(long)]
        root: PathBuf,

        /// Toggle testsigning.
        #[arg(long, value_enum, default_value_t = ToggleArg::On)]
        testsigning: ToggleArg,

        /// Toggle nointegritychecks.
        #[arg(long, value_enum, default_value_t = ToggleArg::On)]
        nointegritychecks: ToggleArg,

        /// Treat any missing store as an error.
        #[arg(long)]
        strict: bool,
    },
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Win7Tree {
            root,
            testsigning,
            nointegritychecks,
            strict,
        } => {
            let opts = bcd_patch::PatchOptions {
                testsigning: testsigning.into(),
                nointegritychecks: nointegritychecks.into(),
            };

            let report = bcd_patch::patch_win7_tree(&root, opts, strict)?;

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
        }
    }

    Ok(())
}
