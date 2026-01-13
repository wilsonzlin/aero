use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::io::BufReader;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "aero-gpu-trace-replay",
    about = "Replay and decode AeroGPU captures (command streams, alloc tables)",
    arg_required_else_help = true
)]
struct Cli {
    /// Legacy mode: replay a full `.aerogputrace` capture.
    #[arg(value_name = "TRACE", conflicts_with = "command")]
    trace: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Decode a raw command stream dump and print a per-packet opcode listing.
    DecodeCmdStream {
        #[arg(value_name = "PATH")]
        path: PathBuf,

        /// Fail on unknown opcodes (default is forward-compatible and prints UNKNOWN).
        #[arg(long)]
        strict: bool,

        /// Emit JSON instead of human-readable text (stable schema for automation).
        #[arg(long)]
        json: bool,
    },

    /// Decode a raw alloc table dump (from `alloc_table_gpa`) and print entries.
    DecodeAllocTable {
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },

    /// Decode a cmd stream + alloc table pair (from a submission) and cross-check mappings.
    DecodeSubmit {
        /// Command stream dump (`cmd_gpa`)
        #[arg(long)]
        cmd: PathBuf,

        /// Alloc table dump (`alloc_table_gpa`)
        #[arg(long)]
        alloc: PathBuf,
    },

    /// Replay a full `.aerogputrace` capture and print presented frame hashes.
    ReplayTrace {
        #[arg(value_name = "TRACE")]
        path: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::DecodeCmdStream { path, strict, json }) => {
            decode_cmd_stream_cmd(path, strict, json)
        }
        Some(Command::DecodeAllocTable { path }) => decode_alloc_table_cmd(path),
        Some(Command::DecodeSubmit { cmd, alloc }) => decode_submit_cmd(cmd, alloc),
        Some(Command::ReplayTrace { path }) => replay_trace_cmd(path),
        None => replay_trace_cmd(cli.trace.expect("clap should require TRACE")),
    }
}

fn decode_cmd_stream_cmd(path: PathBuf, strict: bool, json: bool) -> Result<()> {
    let bytes = fs::read(&path).with_context(|| format!("read cmd stream {}", path.display()))?;
    if json {
        let listing = aero_gpu_trace_replay::cmd_stream_decode::render_cmd_stream_listing(
            &bytes,
            aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingFormat::Json,
        )
        .with_context(|| format!("decode cmd stream {}", path.display()))?;
        print!("{listing}");
        return Ok(());
    }

    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, strict)
        .with_context(|| format!("decode cmd stream {}", path.display()))?;
    print!("{listing}");
    Ok(())
}

fn replay_trace_cmd(path: PathBuf) -> Result<()> {
    let file = fs::File::open(&path).with_context(|| format!("open trace {}", path.display()))?;
    let frames =
        aero_gpu_trace_replay::replay_trace(BufReader::new(file)).context("replay trace")?;
    for frame in frames {
        println!(
            "frame {}: {}x{} sha256={}",
            frame.frame_index,
            frame.width,
            frame.height,
            frame.sha256()
        );
    }
    Ok(())
}

fn decode_alloc_table_cmd(path: PathBuf) -> Result<()> {
    let bytes = fs::read(&path).with_context(|| format!("read alloc table {}", path.display()))?;
    let entries = aero_gpu_trace_replay::alloc_table_dump::decode_alloc_table_entries_le(&bytes)
        .with_context(|| format!("decode alloc table {}", path.display()))?;
    for e in entries {
        println!(
            "alloc_id={} gpa=0x{:016x} size_bytes={} flags=0x{:08x}",
            e.alloc_id, e.gpa, e.size_bytes, e.flags
        );
    }
    Ok(())
}

fn decode_submit_cmd(cmd_path: PathBuf, alloc_path: PathBuf) -> Result<()> {
    let cmd_bytes =
        fs::read(&cmd_path).with_context(|| format!("read cmd stream {}", cmd_path.display()))?;
    let alloc_bytes = fs::read(&alloc_path)
        .with_context(|| format!("read alloc table {}", alloc_path.display()))?;

    let report = aero_gpu_trace_replay::submit_decode::decode_submit(&cmd_bytes, &alloc_bytes)
        .with_context(|| {
            format!(
                "decode-submit --cmd {} --alloc {}",
                cmd_path.display(),
                alloc_path.display()
            )
        })?;

    // Print the alloc references the cmd stream makes, resolved via the alloc table.
    for r in &report.backing_alloc_refs {
        if r.backing_alloc_id == 0 {
            continue;
        }
        let alloc = report
            .alloc_map
            .get(&r.backing_alloc_id)
            .expect("decode_submit should have validated alloc presence");
        println!(
            "cmd@0x{:x} {:?} handle={} backing_alloc_id={} gpa=0x{:016x}",
            r.cmd_offset, r.opcode, r.resource_handle, r.backing_alloc_id, alloc.gpa
        );
    }
    Ok(())
}
