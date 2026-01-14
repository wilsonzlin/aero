use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "aero-gpu-trace-replay",
    about = "Replay and decode AeroGPU captures (command streams, alloc tables)",
    arg_required_else_help = true
)]
struct Cli {
    /// Replay only a single frame index.
    #[arg(long, global = true, value_name = "N")]
    frame: Option<u32>,

    /// Dump replayed frame(s) as `frame_<idx>.png` into this directory.
    #[arg(long, global = true, value_name = "DIR")]
    dump_png: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,

    /// Legacy mode: replay a full `.aerogputrace` capture.
    #[arg(value_name = "TRACE")]
    trace: Option<PathBuf>,
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
    let Cli {
        trace,
        frame,
        dump_png,
        command,
    } = Cli::parse();
 
    match command {
        Some(Command::DecodeCmdStream { path, strict, json }) => {
            ensure_no_replay_flags(frame, &dump_png)?;
            decode_cmd_stream_cmd(path, strict, json)
        }
        Some(Command::DecodeAllocTable { path }) => {
            ensure_no_replay_flags(frame, &dump_png)?;
            decode_alloc_table_cmd(path)
        }
        Some(Command::DecodeSubmit { cmd, alloc }) => {
            ensure_no_replay_flags(frame, &dump_png)?;
            decode_submit_cmd(cmd, alloc)
        }
        Some(Command::ReplayTrace { path }) => replay_trace_cmd(path, frame, dump_png),
        None => {
            let path = match trace {
                Some(path) => path,
                None => {
                    Cli::command()
                        .error(
                            clap::error::ErrorKind::MissingRequiredArgument,
                            "TRACE is required unless a subcommand is provided",
                        )
                        .exit();
                }
            };
            replay_trace_cmd(path, frame, dump_png)
        }
    }
}

fn ensure_no_replay_flags(frame: Option<u32>, dump_png: &Option<PathBuf>) -> Result<()> {
    if frame.is_some() || dump_png.is_some() {
        bail!("--frame/--dump-png can only be used when replaying a trace");
    }
    Ok(())
}

fn decode_cmd_stream_cmd(path: PathBuf, strict: bool, json: bool) -> Result<()> {
    let bytes = fs::read(&path).with_context(|| format!("read cmd stream {}", path.display()))?;
    if json {
        let report = aero_gpu_trace_replay::cmd_stream_decode::decode_cmd_stream(&bytes)
            .with_context(|| format!("decode cmd stream {}", path.display()))?;

        if strict {
            for rec in &report.records {
                if let aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingRecord::Packet(
                    pkt,
                ) = rec
                {
                    if pkt.opcode.is_none() {
                        bail!(
                            "unknown opcode_id=0x{:08X} at offset 0x{:08X}",
                            pkt.opcode_u32,
                            pkt.offset
                        );
                    }
                }
            }
        }

        let listing = report
            .to_json_pretty()
            .with_context(|| format!("encode cmd stream json {}", path.display()))?;
        print!("{listing}");
        return Ok(());
    }
 
    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, strict)
        .with_context(|| format!("decode cmd stream {}", path.display()))?;
    print!("{listing}");
    Ok(())
}

fn replay_trace_cmd(path: PathBuf, frame_filter: Option<u32>, dump_png: Option<PathBuf>) -> Result<()> {
    if let Some(dir) = dump_png.as_ref() {
        fs::create_dir_all(dir)
            .with_context(|| format!("create output dir {}", dir.display()))?;
    }
 
    let file = fs::File::open(&path).with_context(|| format!("open trace {}", path.display()))?;
    let frames = if let Some(frame_idx) = frame_filter {
        aero_gpu_trace_replay::replay_trace_filtered(BufReader::new(file), Some(frame_idx))
            .context("replay trace")?
    } else {
        aero_gpu_trace_replay::replay_trace(BufReader::new(file)).context("replay trace")?
    };
 
    if let Some(frame_idx) = frame_filter {
        if frames.is_empty() {
            bail!("requested frame {frame_idx} was not presented in this trace");
        }
    }
 
    for frame in frames {
        if let Some(dir) = dump_png.as_ref() {
            let out_path = dir.join(format!("frame_{}.png", frame.frame_index));
            write_frame_png(&out_path, &frame)
                .with_context(|| format!("write {}", out_path.display()))?;
        }
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

fn write_frame_png(path: &Path, frame: &aero_gpu_trace_replay::ReplayedFrame) -> Result<()> {
    let file = fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    let w = BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&frame.rgba8)?;
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
