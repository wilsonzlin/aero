#![forbid(unsafe_code)]

// This crate is a native-only CLI tool, but the workspace CI builds `--target wasm32-unknown-unknown
// --workspace --tests --no-run` to ensure the emulator crates remain wasm-compatible.
//
// Provide a tiny wasm32 stub `main` so the workspace continues to compile for wasm targets. The CLI
// is not expected to run in the browser.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::fs::File;
    use std::io::{self, BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use aero_machine::{Machine, MachineConfig, RunExit};
    use aero_storage::{RawDisk, StdFileBackend, VirtualDisk, SECTOR_SIZE};
    use anyhow::{anyhow, bail, Context, Result};
    use clap::{ArgGroup, Parser};

    const SLICE_INST_BUDGET: u64 = 100_000;

    #[derive(Debug, Parser)]
    #[command(
        about = "Native runner for aero_machine::Machine (boot/integration debugging)",
        group(
            ArgGroup::new("stop")
                .required(true)
                .args(["max_insts", "max_ms"])
        )
    )]
    pub struct Args {
        /// Raw disk image to attach (must be a multiple of 512 bytes).
        #[arg(long)]
        disk: PathBuf,

        /// Open the disk image read-only (guest writes will fail).
        #[arg(long)]
        disk_ro: bool,

        /// Guest RAM size in MiB.
        #[arg(long, default_value_t = 64)]
        ram: u64,

        /// Stop after executing at most N guest instructions.
        #[arg(long)]
        max_insts: Option<u64>,

        /// Stop after running for at most N milliseconds of host time.
        #[arg(long)]
        max_ms: Option<u64>,

        /// Where to write accumulated COM1 output bytes (`stdout` or a file path).
        #[arg(long, default_value = "stdout")]
        serial_out: String,

        /// Dump the last VGA framebuffer to a PNG file on exit.
        #[arg(long)]
        vga_png: Option<PathBuf>,

        /// Save a snapshot (aero_snapshot format) on exit.
        #[arg(long)]
        snapshot_save: Option<PathBuf>,

        /// Load a snapshot (aero_snapshot format) before running.
        #[arg(long)]
        snapshot_load: Option<PathBuf>,
    }

    pub fn main() -> Result<()> {
        let args = Args::parse();

        let ram_bytes = args
            .ram
            .checked_mul(1024 * 1024)
            .context("RAM size overflow")?;

        // Use the canonical PC platform defaults so the CLI is useful for full-system boot images.
        let mut machine = Machine::new(MachineConfig::win7_storage_defaults(ram_bytes))
            .map_err(|e| anyhow!("{e}"))?;

        let disk_backend = open_disk_backend(&args.disk, args.disk_ro)?;
        machine
            .set_disk_backend(disk_backend)
            .map_err(|e| anyhow!("{e}"))?;
        // `Machine::new` performs an initial BIOS POST + boot attempt. Re-run POST after attaching
        // the user's disk so the guest starts executing from the boot sector.
        machine.reset();

        if let Some(path) = &args.snapshot_load {
            let mut f = File::open(path)
                .with_context(|| format!("failed to open snapshot for load: {}", path.display()))?;
            machine
                .restore_snapshot_from_checked(&mut f)
                .map_err(|e| anyhow!("{e}"))?;

            // Storage controller snapshots intentionally drop host backends. Reattach the shared
            // disk so the guest can continue booting after restore.
            machine
                .attach_shared_disk_to_ahci_port0()
                .context("failed to reattach shared disk to AHCI port0")?;
            machine
                .attach_shared_disk_to_virtio_blk()
                .map_err(|e| anyhow!("{e}"))?;
        }

        let mut serial_sink = open_serial_sink(&args.serial_out)?;

        let start = Instant::now();
        let mut total_executed: u64 = 0;

        loop {
            let exit = if let Some(max_insts) = args.max_insts {
                if total_executed >= max_insts {
                    break;
                }
                let budget = (max_insts - total_executed).min(SLICE_INST_BUDGET);
                machine.run_slice(budget)
            } else {
                let max_ms = args
                    .max_ms
                    .expect("clap enforces that one of max_insts/max_ms is set");
                if start.elapsed() >= Duration::from_millis(max_ms) {
                    break;
                }
                machine.run_slice(SLICE_INST_BUDGET)
            };

            total_executed = total_executed.saturating_add(exit.executed());
            stream_serial(&mut machine, &mut serial_sink)?;

            match handle_exit(&mut machine, exit, total_executed)? {
                LoopControl::Continue => continue,
                LoopControl::Break => break,
            }
        }

        // Flush any remaining serial bytes.
        stream_serial(&mut machine, &mut serial_sink)?;
        serial_sink.flush()?;

        if let Some(path) = &args.snapshot_save {
            let mut f = File::create(path).with_context(|| {
                format!("failed to create snapshot file for save: {}", path.display())
            })?;
            machine
                .save_snapshot_full_to(&mut f)
                .map_err(|e| anyhow!("{e}"))?;
        }

        if let Some(path) = &args.vga_png {
            dump_vga_png(&mut machine, path)?;
        }

        Ok(())
    }

    fn open_disk_backend(path: &Path, read_only: bool) -> Result<Box<dyn VirtualDisk + Send>> {
        let meta = std::fs::metadata(path)
            .with_context(|| format!("failed to stat disk image: {}", path.display()))?;
        let len = meta.len();
        if len == 0 {
            bail!("disk image is empty (expected at least one 512-byte sector)");
        }
        if len % SECTOR_SIZE as u64 != 0 {
            bail!(
                "disk image length {} is not a multiple of {} bytes",
                len,
                SECTOR_SIZE
            );
        }
        let backend = if read_only {
            StdFileBackend::open_read_only(path)
        } else {
            StdFileBackend::open_rw(path)
        }
        .map_err(|e| anyhow!("failed to open disk image {}: {e}", path.display()))?;
        let disk = RawDisk::open(backend)
            .map_err(|e| anyhow!("failed to open raw disk backend {}: {e}", path.display()))?;
        Ok(Box::new(disk))
    }

    fn open_serial_sink(serial_out: &str) -> Result<Box<dyn Write>> {
        if serial_out == "stdout" {
            return Ok(Box::new(io::stdout()));
        }
        let f = File::create(serial_out)
            .with_context(|| format!("failed to create serial output file: {serial_out}"))?;
        Ok(Box::new(BufWriter::new(f)))
    }

    fn stream_serial(machine: &mut Machine, out: &mut dyn Write) -> Result<()> {
        let bytes = machine.take_serial_output();
        if !bytes.is_empty() {
            out.write_all(&bytes)?;
        }
        Ok(())
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum LoopControl {
        Continue,
        Break,
    }

    fn handle_exit(
        machine: &mut Machine,
        exit: RunExit,
        total_executed: u64,
    ) -> Result<LoopControl> {
        match exit {
            RunExit::Completed { .. } => Ok(LoopControl::Continue),
            RunExit::Halted { .. } => {
                eprintln!("guest halted after {total_executed} instructions");
                Ok(LoopControl::Break)
            }
            RunExit::ResetRequested { kind, .. } => {
                eprintln!("guest requested reset: {kind:?} (continuing)");
                machine.reset();
                Ok(LoopControl::Continue)
            }
            RunExit::Assist { reason, .. } => {
                bail!("execution stopped: assist required: {reason:?}")
            }
            RunExit::Exception { exception, .. } => {
                bail!("execution stopped: exception: {exception:?}")
            }
            RunExit::CpuExit { exit, .. } => bail!("execution stopped: cpu exit: {exit:?}"),
        }
    }

    fn dump_vga_png(machine: &mut Machine, path: &Path) -> Result<()> {
        machine.display_present();
        let (w, h) = machine.display_resolution();
        if w == 0 || h == 0 {
            bail!("no VGA framebuffer available (resolution was {w}x{h})");
        }

        let fb = machine.display_framebuffer();
        let expected_len = (w as usize)
            .checked_mul(h as usize)
            .context("framebuffer size overflow")?;
        if fb.len() != expected_len {
            bail!(
                "unexpected framebuffer length: got {}, expected {} ({w}x{h})",
                fb.len(),
                expected_len
            );
        }

        // `aero_gpu_vga` framebuffer pixels are u32 with little-endian RGBA byte order:
        //   value = R | (G<<8) | (B<<16) | (A<<24)
        // Convert to an explicit RGBA byte buffer for the `image` crate.
        let mut rgba = Vec::with_capacity(fb.len() * 4);
        for &p in fb {
            rgba.push((p & 0xFF) as u8); // R
            rgba.push(((p >> 8) & 0xFF) as u8); // G
            rgba.push(((p >> 16) & 0xFF) as u8); // B
            rgba.push(((p >> 24) & 0xFF) as u8); // A
        }

        let img =
            image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| anyhow!("invalid image data"))?;
        img.save(path)
            .with_context(|| format!("failed to write PNG: {}", path.display()))?;
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    native::main()
}
