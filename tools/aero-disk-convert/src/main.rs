#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::path::PathBuf;

    use clap::{Args, Parser, Subcommand};

    use aero_disk_convert::{
        convert, ConvertOptions, OutputFormat, DEFAULT_AEROSPARSE_BLOCK_SIZE_BYTES,
    };

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

    pub(super) fn main() -> anyhow::Result<()> {
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
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    native::main()
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // This binary is a host-only developer tool. It relies on OS filesystem access and isn't
    // supported on wasm32. We still provide a stub so `cargo test --target wasm32-unknown-unknown
    // --workspace --tests --no-run` can compile the workspace without special-casing tools.
    panic!("aero-disk-convert is not supported on wasm32 targets");
}
