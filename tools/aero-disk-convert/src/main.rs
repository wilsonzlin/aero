use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use aero_disk_convert::{convert, ConvertOptions, OutputFormat, DEFAULT_AEROSPARSE_BLOCK_SIZE_BYTES};

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Convert a disk image between supported formats.
    Convert(ConvertArgs),
}

#[derive(Args, Debug)]
struct ConvertArgs {
    /// Input disk image path (raw/qcow2/vhd/aerosparse).
    #[arg(long)]
    input: PathBuf,

    /// Output disk image path.
    #[arg(long)]
    output: PathBuf,

    /// Output format.
    #[arg(long, value_enum)]
    output_format: OutputFormat,

    /// Allocation block size (AeroSparse output only).
    #[arg(long, default_value_t = DEFAULT_AEROSPARSE_BLOCK_SIZE_BYTES)]
    block_size_bytes: u32,

    /// Show a progress bar.
    #[arg(long)]
    progress: bool,

    /// Overwrite the output file if it already exists.
    #[arg(long)]
    force: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Convert(args) => convert(ConvertOptions {
            input: args.input,
            output: args.output,
            output_format: args.output_format,
            block_size_bytes: args.block_size_bytes,
            progress: args.progress,
            force: args.force,
        })?,
    }

    Ok(())
}

