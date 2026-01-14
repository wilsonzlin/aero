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

    use aero_machine::{BootDevice, Machine, MachineConfig, RunExit};
    use aero_storage::{AeroCowDisk, DiskImage, StdFileBackend, VirtualDisk, SECTOR_SIZE};
    use anyhow::{anyhow, bail, Context, Result};
    use clap::{ArgGroup, Parser, ValueEnum};

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
        /// Disk image to attach (raw/qcow2/vhd/aerospar; auto-detected).
        ///
        /// The virtual capacity must be a multiple of 512 bytes.
        #[arg(long)]
        disk: PathBuf,

        /// Open the disk image read-only (guest writes will fail).
        #[arg(long, conflicts_with = "disk_overlay")]
        disk_ro: bool,

        /// Optional copy-on-write overlay image (AEROSPAR).
        ///
        /// If provided, the base `--disk` is opened read-only and guest writes go to the overlay.
        #[arg(long)]
        disk_overlay: Option<PathBuf>,

        /// Allocation unit (block size) used when creating a new `--disk-overlay` (bytes).
        ///
        /// Must be a power of two and a multiple of 512.
        #[arg(long, default_value_t = 1024 * 1024, requires = "disk_overlay")]
        disk_overlay_block_size: u32,

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

        /// Where to write accumulated DebugCon output bytes (I/O port `0xE9`).
        ///
        /// Use `none` to disable (default), `stdout` to write to stdout, or a file path.
        #[arg(long, default_value = "none")]
        debugcon_out: String,

        /// Dump the last VGA framebuffer to a PNG file on exit.
        #[arg(long)]
        vga_png: Option<PathBuf>,

        /// Save a snapshot (aero_snapshot format) on exit.
        #[arg(long)]
        snapshot_save: Option<PathBuf>,

        /// Load a snapshot (aero_snapshot format) before running.
        #[arg(long)]
        snapshot_load: Option<PathBuf>,

        /// Optional install/recovery ISO to attach as an ATAPI CD-ROM (IDE secondary master).
        ///
        /// This uses the canonical Win7 install-media slot (`disk_id=1`).
        #[arg(long)]
        install_iso: Option<PathBuf>,

        /// BIOS boot selection policy.
        ///
        /// Defaults to:
        /// - `hdd` when no `--install-iso` is provided
        /// - `cd-first` when `--install-iso` is provided
        ///
        /// Note: when `--snapshot-load` is used, this only affects future guest resets. The current
        /// CPU state is restored from the snapshot and the VM is not reset.
        #[arg(long, value_enum)]
        boot: Option<BootMode>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
    enum BootMode {
        /// Boot from the primary HDD (`DL=0x80`).
        Hdd,
        /// Boot from the install-media CD-ROM (`DL=0xE0`).
        Cdrom,
        /// Enable firmware "CD-first when present" policy (try `DL=0xE0` when ISO is present, otherwise fall back to HDD).
        CdFirst,
    }

    pub fn main() -> Result<()> {
        let args = Args::parse();

        let ram_bytes = args
            .ram
            .checked_mul(1024 * 1024)
            .context("RAM size overflow")?;

        // By default, treat an attached install ISO as a request to boot from CD once, then allow
        // the guest to reboot into HDD without host-side boot-drive toggling (mirrors the browser
        // runtime's `vmRuntime="machine"` install flow).
        let boot_mode = args.boot.unwrap_or_else(|| {
            if args.install_iso.is_some() {
                BootMode::CdFirst
            } else {
                BootMode::Hdd
            }
        });
        if matches!(boot_mode, BootMode::Cdrom | BootMode::CdFirst) && args.install_iso.is_none() {
            bail!("--boot={boot_mode:?} requires --install-iso");
        }

        // Use the canonical PC platform defaults so the CLI is useful for full-system boot images.
        let mut machine = Machine::new(MachineConfig::win7_storage_defaults(ram_bytes))
            .map_err(|e| anyhow!("{e}"))?;

        // Record the host's chosen disk paths in the machine's snapshot overlay refs so snapshots
        // produced by this CLI remain self-describing (even when no explicit COW overlay is used).
        //
        // Note: these refs are metadata only; disk bytes always remain external to the snapshot
        // blob.
        let base_image = args.disk.display().to_string();
        if let Some(overlay) = &args.disk_overlay {
            machine
                .set_ahci_port0_disk_overlay_ref(base_image.clone(), overlay.display().to_string());
        } else {
            machine.set_ahci_port0_disk_overlay_ref(base_image.clone(), "");
        }

        let disk_backend = if let Some(overlay) = &args.disk_overlay {
            open_disk_backend_with_overlay(&args.disk, overlay, args.disk_overlay_block_size)?
        } else {
            open_disk_backend(&args.disk, args.disk_ro)?
        };
        machine
            .set_disk_backend(disk_backend)
            .map_err(|e| anyhow!("{e}"))?;

        // Track whether the firmware "CD-first when present" policy is enabled so we can disable it
        // after the first guest-initiated reset (Windows setup reboots into the installed HDD while
        // leaving install media inserted).
        let mut cd_first_enabled: bool;

        if let Some(path) = &args.snapshot_load {
            let mut f = File::open(path)
                .with_context(|| format!("failed to open snapshot for load: {}", path.display()))?;
            machine
                .restore_snapshot_from_checked(&mut f)
                .map_err(|e| anyhow!("{e}"))?;

            // Snapshot disk refs are host-managed metadata. Warn if the snapshot was produced for a
            // different base/overlay path than the current CLI flags.
            if let Some(restored) = machine.restored_disk_overlays() {
                if let Some(primary) = restored
                    .disks
                    .iter()
                    .find(|d| d.disk_id == Machine::DISK_ID_PRIMARY_HDD)
                {
                    if !primary.base_image.is_empty() && primary.base_image != base_image {
                        eprintln!(
                            "warning: snapshot base_image differs from --disk: snapshot={} cli={}",
                            primary.base_image, base_image
                        );
                    }
                    if !primary.overlay_image.is_empty() {
                        match &args.disk_overlay {
                            Some(cli_overlay) => {
                                let cli_overlay = cli_overlay.display().to_string();
                                if primary.overlay_image != cli_overlay {
                                    eprintln!(
                                        "warning: snapshot overlay_image differs from --disk-overlay: snapshot={} cli={}",
                                        primary.overlay_image, cli_overlay
                                    );
                                }
                            }
                            None => {
                                eprintln!(
                                    "warning: snapshot specifies overlay_image {} but CLI did not provide --disk-overlay",
                                    primary.overlay_image
                                );
                            }
                        }
                    }
                }

                if let Some(install_iso) = &args.install_iso {
                    let cli_iso = install_iso.display().to_string();
                    if let Some(cd) = restored
                        .disks
                        .iter()
                        .find(|d| d.disk_id == Machine::DISK_ID_INSTALL_MEDIA)
                    {
                        if !cd.base_image.is_empty() && cd.base_image != cli_iso {
                            eprintln!(
                                "warning: snapshot install-media base_image differs from --install-iso: snapshot={} cli={}",
                                cd.base_image, cli_iso
                            );
                        }
                        if !cd.overlay_image.is_empty() {
                            eprintln!(
                                "warning: snapshot install-media overlay_image is non-empty (expected read-only ISO): {}",
                                cd.overlay_image
                            );
                        }
                    }
                }
            }

            // Storage controller snapshots intentionally drop host backends. Reattach the shared
            // disk so the guest can continue booting after restore.
            machine
                .attach_shared_disk_to_ahci_port0()
                .context("failed to reattach shared disk to AHCI port0")?;
            machine
                .attach_shared_disk_to_virtio_blk()
                .map_err(|e| anyhow!("{e}"))?;

            // If install media is provided, reattach it without changing guest-visible tray state.
            if let Some(iso_path) = &args.install_iso {
                let iso = open_disk_image(iso_path, true)?;
                machine
                    .attach_install_media_iso_for_restore(Box::new(iso))
                    .with_context(|| {
                        format!(
                            "failed to attach install ISO for restore: {}",
                            iso_path.display()
                        )
                    })?;
                machine
                    .set_ide_secondary_master_atapi_overlay_ref(iso_path.display().to_string(), "");
            }

            // Only override the snapshot's boot policy when explicitly requested. Otherwise we keep
            // the restored BIOS config intact.
            if args.boot.is_some() {
                match boot_mode {
                    BootMode::Hdd => {
                        machine.set_boot_from_cd_if_present(false);
                        machine.set_boot_drive(0x80);
                    }
                    BootMode::Cdrom => {
                        machine.set_boot_from_cd_if_present(false);
                        machine.set_boot_drive(0xE0);
                    }
                    BootMode::CdFirst => {
                        machine.set_cd_boot_drive(0xE0);
                        machine.set_boot_from_cd_if_present(true);
                        machine.set_boot_drive(0x80);
                    }
                }
            }
            cd_first_enabled = machine.boot_from_cd_if_present();
        } else {
            // No snapshot restore: attach optional install media and apply boot policy, then reset.
            if let Some(iso_path) = &args.install_iso {
                let iso = open_disk_image(iso_path, true)?;
                machine
                    .attach_install_media_iso_and_set_overlay_ref(
                        Box::new(iso),
                        iso_path.display().to_string(),
                    )
                    .with_context(|| {
                        format!("failed to attach install ISO: {}", iso_path.display())
                    })?;
            }

            match boot_mode {
                BootMode::Hdd => {
                    machine.set_boot_from_cd_if_present(false);
                    machine.set_boot_drive(0x80);
                }
                BootMode::Cdrom => {
                    machine.set_boot_from_cd_if_present(false);
                    machine.set_boot_drive(0xE0);
                }
                BootMode::CdFirst => {
                    machine.set_cd_boot_drive(0xE0);
                    machine.set_boot_from_cd_if_present(true);
                    machine.set_boot_drive(0x80);
                }
            }
            cd_first_enabled = machine.boot_from_cd_if_present();

            // `Machine::new` performs an initial BIOS POST + boot attempt. Re-run POST after
            // attaching disks and configuring boot policy so the guest starts executing from the
            // selected boot device.
            machine.reset();
        }

        let mut serial_sink = open_serial_sink(&args.serial_out)?;
        let mut debugcon_sink = open_optional_sink(&args.debugcon_out)?;

        let start = Instant::now();
        let mut total_executed: u64 = 0;
        let mut run_error: Option<anyhow::Error> = None;

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
            if let Some(out) = debugcon_sink.as_mut() {
                stream_debugcon(&mut machine, out)?;
            }

            match handle_exit(&mut machine, exit, total_executed, &mut cd_first_enabled) {
                Ok(LoopControl::Continue) => continue,
                Ok(LoopControl::Break) => break,
                Err(e) => {
                    run_error = Some(e);
                    break;
                }
            }
        }

        // Flush any remaining serial bytes.
        stream_serial(&mut machine, &mut serial_sink)?;
        if let Err(e) = serial_sink.flush() {
            if run_error.is_some() {
                eprintln!("warning: failed to flush serial output: {e}");
            } else {
                return Err(e.into());
            }
        }
        if let Some(out) = debugcon_sink.as_mut() {
            stream_debugcon(&mut machine, out)?;
            if let Err(e) = out.flush() {
                if run_error.is_some() {
                    eprintln!("warning: failed to flush debugcon output: {e}");
                } else {
                    return Err(e.into());
                }
            }
        }

        if let Some(path) = &args.snapshot_save {
            let mut f = File::create(path).with_context(|| {
                format!(
                    "failed to create snapshot file for save: {}",
                    path.display()
                )
            })?;
            if let Err(e) = machine
                .save_snapshot_full_to(&mut f)
                .map_err(|e| anyhow!("{e}"))
            {
                if run_error.is_some() {
                    eprintln!(
                        "warning: failed to save snapshot to {}: {e}",
                        path.display()
                    );
                } else {
                    return Err(e);
                }
            }
        }

        if let Some(path) = &args.vga_png {
            if let Err(e) = dump_vga_png(&mut machine, path) {
                if run_error.is_some() {
                    eprintln!("warning: failed to dump VGA PNG to {}: {e}", path.display());
                } else {
                    return Err(e);
                }
            }
        }

        if let Some(e) = run_error {
            return Err(e);
        }

        Ok(())
    }

    fn open_disk_image(path: &Path, read_only: bool) -> Result<DiskImage<StdFileBackend>> {
        let backend = if read_only {
            StdFileBackend::open_read_only(path)
        } else {
            StdFileBackend::open_rw(path)
        }
        .map_err(|e| anyhow!("failed to open disk image {}: {e}", path.display()))?;

        let disk = DiskImage::open_auto(backend)
            .map_err(|e| anyhow!("failed to open disk image {}: {e}", path.display()))?;

        let capacity = disk.capacity_bytes();
        if capacity == 0 {
            bail!(
                "disk image is empty (expected at least one {}-byte sector)",
                SECTOR_SIZE
            );
        }
        if capacity % SECTOR_SIZE as u64 != 0 {
            bail!(
                "disk image capacity {} is not a multiple of {} bytes",
                capacity,
                SECTOR_SIZE
            );
        }

        Ok(disk)
    }

    fn open_disk_backend(path: &Path, read_only: bool) -> Result<Box<dyn VirtualDisk>> {
        let disk = open_disk_image(path, read_only)?;
        Ok(Box::new(disk))
    }

    fn open_disk_backend_with_overlay(
        base_path: &Path,
        overlay_path: &Path,
        create_block_size: u32,
    ) -> Result<Box<dyn VirtualDisk>> {
        let base = open_disk_image(base_path, true)?;

        let overlay_exists = overlay_path.exists();
        let overlay_backend = if overlay_exists {
            StdFileBackend::open_rw(overlay_path)
        } else {
            StdFileBackend::create(overlay_path, 0)
        }
        .map_err(|e| {
            anyhow!(
                "failed to open overlay image {}: {e}",
                overlay_path.display()
            )
        })?;

        let cow = if overlay_exists {
            AeroCowDisk::open(base, overlay_backend)
        } else {
            AeroCowDisk::create(base, overlay_backend, create_block_size)
        }
        .map_err(|e| anyhow!("failed to initialize COW overlay disk: {e}"))?;

        Ok(Box::new(cow))
    }

    fn open_serial_sink(serial_out: &str) -> Result<Box<dyn Write>> {
        if serial_out == "stdout" {
            return Ok(Box::new(io::stdout()));
        }
        let f = File::create(serial_out)
            .with_context(|| format!("failed to create serial output file: {serial_out}"))?;
        Ok(Box::new(BufWriter::new(f)))
    }

    fn open_optional_sink(dest: &str) -> Result<Option<Box<dyn Write>>> {
        if dest == "none" {
            return Ok(None);
        }
        Ok(Some(open_serial_sink(dest)?))
    }

    fn stream_serial(machine: &mut Machine, out: &mut dyn Write) -> Result<()> {
        let bytes = machine.take_serial_output();
        if !bytes.is_empty() {
            out.write_all(&bytes)?;
        }
        Ok(())
    }

    fn stream_debugcon(machine: &mut Machine, out: &mut dyn Write) -> Result<()> {
        let bytes = machine.take_debugcon_output();
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
        cd_first_enabled: &mut bool,
    ) -> Result<LoopControl> {
        match exit {
            RunExit::Completed { .. } => Ok(LoopControl::Continue),
            RunExit::Halted { .. } => {
                eprintln!("guest halted after {total_executed} instructions");
                Ok(LoopControl::Break)
            }
            RunExit::ResetRequested { kind, .. } => {
                // When using the "CD-first when present" policy, Windows setup commonly boots from
                // CD once, then reboots into the installed HDD while leaving the ISO attached.
                // Disable the policy after the first guest reset so setup does not loop back into
                // install media.
                if *cd_first_enabled && machine.active_boot_device() == BootDevice::Cdrom {
                    eprintln!(
                        "guest requested reset: {kind:?} (disabling CD-first policy; booting HDD next)"
                    );
                    machine.set_boot_from_cd_if_present(false);
                    machine.set_boot_drive(0x80);
                    *cd_first_enabled = false;
                } else {
                    eprintln!("guest requested reset: {kind:?} (continuing)");
                }
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
