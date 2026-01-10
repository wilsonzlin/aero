use crate::deps::DepContext;
use anyhow::{anyhow, Context, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use walkdir::WalkDir;

pub struct IsoExtractor {
    kind: IsoExtractorKind,
}

enum IsoExtractorKind {
    SevenZip { exe: PathBuf },
    Xorriso { exe: PathBuf },
    PowerShellMount { exe: PathBuf },
}

impl IsoExtractor {
    pub fn detect(ctx: &DepContext) -> Result<Self> {
        if let Some(exe) = ctx.seven_zip.clone() {
            return Ok(Self {
                kind: IsoExtractorKind::SevenZip { exe },
            });
        }
        if let Some(exe) = ctx.xorriso.clone() {
            return Ok(Self {
                kind: IsoExtractorKind::Xorriso { exe },
            });
        }
        if cfg!(windows) {
            if let Some(exe) = ctx.powershell.clone() {
                return Ok(Self {
                    kind: IsoExtractorKind::PowerShellMount { exe },
                });
            }
        }
        Err(anyhow!(
            "No ISO extractor detected (need 7z or xorriso; on Windows, PowerShell mount can be used as a last resort)"
        ))
    }

    pub fn extract(&self, input_iso: &Path, dest_dir: &Path, verbose: bool) -> Result<()> {
        match &self.kind {
            IsoExtractorKind::SevenZip { exe } => {
                run(
                    Command::new(exe)
                        .arg("x")
                        .arg("-y")
                        .arg(format!("-o{}", dest_dir.display()))
                        .arg(input_iso),
                    verbose,
                )
                .context("7z extraction failed")
            }
            IsoExtractorKind::Xorriso { exe } => {
                run(
                    Command::new(exe)
                        .arg("-osirrox")
                        .arg("on")
                        .arg("-indev")
                        .arg(input_iso)
                        .arg("-extract")
                        .arg("/")
                        .arg(dest_dir),
                    verbose,
                )
                .context("xorriso extraction failed")
            }
            IsoExtractorKind::PowerShellMount { exe } => {
                let script = r#"
param([string]$Iso,[string]$Dest)
$ErrorActionPreference = "Stop"
$isoPath = (Resolve-Path $Iso).Path
New-Item -ItemType Directory -Force -Path $Dest | Out-Null
$diskImage = Mount-DiskImage -ImagePath $isoPath -PassThru
try {
  $vol = $diskImage | Get-Volume
  $driveLetter = $vol.DriveLetter
  if (-not $driveLetter) { throw "Failed to determine mounted ISO drive letter" }
  $src = "$driveLetter`:\"
  Copy-Item -Path (Join-Path $src '*') -Destination $Dest -Recurse -Force
} finally {
  Dismount-DiskImage -ImagePath $isoPath | Out-Null
}
"#;

                let script_file = tempfile::Builder::new()
                    .prefix("aero-win7-slipstream-extract-")
                    .suffix(".ps1")
                    .tempfile()
                    .context("Failed to create temporary PowerShell script")?;
                std::fs::write(script_file.path(), script)
                    .context("Failed to write temporary PowerShell script")?;

                run(
                    Command::new(exe)
                        .arg("-NoProfile")
                        .arg("-ExecutionPolicy")
                        .arg("Bypass")
                        .arg("-File")
                        .arg(script_file.path())
                        .arg("-Iso")
                        .arg(input_iso)
                        .arg("-Dest")
                        .arg(dest_dir),
                    verbose,
                )
                .context("PowerShell mount+copy extraction failed")
            }
        }
    }
}

pub struct IsoBuilder {
    kind: IsoBuilderKind,
    xorriso_for_label: Option<PathBuf>,
}

/// ISO extractors often preserve the "read-only" attribute from ISO9660/UDF metadata.
/// That breaks patching steps that need to modify files in-place (e.g. `boot/BCD`, WIMs).
pub fn make_tree_writable(root: &Path) -> Result<()> {
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::metadata(path)?;
        let mut perms = metadata.permissions();

        #[cfg(windows)]
        {
            if perms.readonly() {
                perms.set_readonly(false);
                fs::set_permissions(path, perms)?;
            }
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = perms.mode();
            let new_mode = mode | 0o200;
            if new_mode != mode {
                perms.set_mode(new_mode);
                fs::set_permissions(path, perms)?;
            }
        }
    }
    Ok(())
}

enum IsoBuilderKind {
    Xorriso { exe: PathBuf },
    Oscdimg { exe: PathBuf },
}

impl IsoBuilder {
    pub fn detect(ctx: &DepContext) -> Result<Self> {
        if cfg!(windows) {
            if let Some(exe) = ctx.oscdimg.clone() {
                return Ok(Self {
                    kind: IsoBuilderKind::Oscdimg { exe },
                    xorriso_for_label: ctx.xorriso.clone(),
                });
            }
        }
        if let Some(exe) = ctx.xorriso.clone() {
            return Ok(Self {
                kind: IsoBuilderKind::Xorriso { exe: exe.clone() },
                xorriso_for_label: Some(exe),
            });
        }
        Err(anyhow!(
            "No ISO builder detected (need xorriso; on Windows, oscdimg from the ADK is preferred)"
        ))
    }

    pub fn build(&self, input_iso: &Path, iso_root: &Path, output_iso: &Path, verbose: bool) -> Result<()> {
        let volume_id = self
            .read_volume_id(input_iso, verbose)
            .unwrap_or_else(|| "WIN7_AERO".to_string());

        let bios_boot = iso_root.join("boot").join("etfsboot.com");
        if !bios_boot.is_file() {
            return Err(anyhow!(
                "Missing BIOS boot image at {} (expected Windows install media layout)",
                bios_boot.display()
            ));
        }

        let uefi_boot_candidates = [
            iso_root
                .join("efi")
                .join("microsoft")
                .join("boot")
                .join("efisys.bin"),
            iso_root.join("efi").join("boot").join("efisys.bin"),
            iso_root
                .join("efi")
                .join("microsoft")
                .join("boot")
                .join("efisys_noprompt.bin"),
        ];
        let uefi_boot = uefi_boot_candidates
            .into_iter()
            .find(|p| p.is_file());

        match &self.kind {
            IsoBuilderKind::Xorriso { exe } => {
                let mut cmd = Command::new(exe);
                cmd.arg("-as")
                    .arg("mkisofs")
                    .arg("-iso-level")
                    .arg("3")
                    .arg("-udf")
                    .arg("-J")
                    .arg("-joliet-long")
                    .arg("-relaxed-filenames")
                    .arg("-V")
                    .arg(&volume_id)
                    .arg("-b")
                    .arg("boot/etfsboot.com")
                    .arg("-no-emul-boot")
                    .arg("-boot-load-size")
                    .arg("8")
                    .arg("-boot-info-table");

                if let Some(uefi_boot) = uefi_boot {
                    let uefi_rel = uefi_boot.strip_prefix(iso_root).unwrap();
                    cmd.arg("-eltorito-alt-boot")
                        .arg("-e")
                        .arg(uefi_rel.as_os_str())
                        .arg("-no-emul-boot");
                }

                cmd.arg("-o").arg(output_iso).arg(iso_root);

                run(&mut cmd, verbose).context("xorriso mkisofs failed")
            }
            IsoBuilderKind::Oscdimg { exe } => {
                let bios_boot_rel = Path::new("boot").join("etfsboot.com");
                let mut cmd = Command::new(exe);
                cmd.arg("-m")
                    .arg("-o")
                    .arg("-u2")
                    .arg("-udfver102")
                    .arg("-h")
                    .arg(format!("-l{}", volume_id));

                if let Some(uefi_boot) = uefi_boot {
                    let uefi_boot_rel = uefi_boot.strip_prefix(iso_root).unwrap_or(&uefi_boot);
                    cmd.arg(format!(
                        "-bootdata:2#p0,e,b{}#pEF,e,b{}",
                        to_oscdimg_path(&bios_boot_rel).display(),
                        to_oscdimg_path(uefi_boot_rel).display()
                    ));
                } else {
                    cmd.arg(format!(
                        "-bootdata:1#p0,e,b{}",
                        to_oscdimg_path(&bios_boot_rel).display()
                    ));
                }

                cmd.arg(iso_root).arg(output_iso);
                run(&mut cmd, verbose).context("oscdimg failed")
            }
        }
    }

    fn read_volume_id(&self, input_iso: &Path, verbose: bool) -> Option<String> {
        let xorriso = self.xorriso_for_label.as_ref()?;
        let out = run_capture(
            Command::new(xorriso)
                .arg("-indev")
                .arg(input_iso)
                .arg("-pvd_info"),
            verbose,
        )
        .ok()?;
        for line in out.lines() {
            if let Some(rest) = line.strip_prefix("Volume id :") {
                let id = rest.trim();
                if !id.is_empty() {
                    return Some(id.to_string());
                }
            }
        }
        None
    }
}

fn to_oscdimg_path(path: &Path) -> PathBuf {
    // oscdimg is Windows-only, but it is picky about backslashes in some environments.
    // Normalize to the platform separator to reduce surprises.
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().replace('/', "\\"))
    } else {
        path.to_path_buf()
    }
}

fn run(cmd: &mut Command, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("> {:?}", cmd);
    }
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to spawn external command")?;
    if !status.success() {
        return Err(anyhow!("External command failed with status: {status}"));
    }
    Ok(())
}

fn run_capture(cmd: &mut Command, verbose: bool) -> Result<String> {
    if verbose {
        eprintln!("> {:?}", cmd);
    }
    let output = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .context("Failed to spawn external command")?;
    if !output.status.success() {
        return Err(anyhow!(
            "External command failed with status: {}",
            output.status
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn _is_iso(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| ext.eq_ignore_ascii_case("iso"))
        .unwrap_or(false)
}
