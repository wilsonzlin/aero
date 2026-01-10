use crate::unattend::UnattendMode;
use crate::wim::{Arch, SigningMode};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "aero-win7-slipstream")]
#[command(version)]
#[command(about = "Slipstream Aero drivers/cert/policy into a user-supplied Windows 7 SP1 ISO")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Patch a Windows 7 SP1 ISO (extract → inject → patch WIM/BCD → rebuild)
    PatchIso(PatchIsoArgs),
    /// Verify that an ISO or extracted tree looks like an Aero-patched Win7 ISO
    VerifyIso(VerifyIsoArgs),
    /// Print detected external dependencies and suggested install commands
    Deps,
}

#[derive(Parser, Debug, Clone)]
pub struct PatchIsoArgs {
    /// Windows 7 SP1 ISO to patch
    #[arg(long)]
    pub input: PathBuf,
    /// Output patched ISO path
    #[arg(long)]
    pub output: PathBuf,
    /// Driver pack root directory (expects x86/ and amd64/ or similar)
    #[arg(long)]
    pub drivers: PathBuf,
    /// Target architecture
    #[arg(long, default_value = "auto")]
    pub arch: ArchChoice,
    /// Unattended install config to generate
    #[arg(long, default_value = "drivers-only")]
    pub unattend: UnattendMode,
    /// Boot policy mode for unsigned/test-signed drivers
    #[arg(long, default_value = "testsigning")]
    pub signing_mode: SigningMode,
    /// Root certificate to inject into offline images when using test-signed drivers
    #[arg(long)]
    pub cert: Option<PathBuf>,
    /// Backend for WIM/registry operations
    #[arg(long, default_value = "auto")]
    pub backend: BackendChoice,
    /// Optional workdir (defaults to a temporary directory)
    #[arg(long)]
    pub workdir: Option<PathBuf>,
    /// Keep workdir for debugging
    #[arg(long)]
    pub keep_workdir: bool,
    /// Verbose logging
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct VerifyIsoArgs {
    /// ISO file or extracted ISO root directory to verify
    #[arg(long)]
    pub input: PathBuf,
    /// Verbose logging
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
pub enum ArchChoice {
    Auto,
    X86,
    X64,
}

impl ArchChoice {
    pub fn to_arch(self) -> Option<Arch> {
        match self {
            ArchChoice::Auto => None,
            ArchChoice::X86 => Some(Arch::X86),
            ArchChoice::X64 => Some(Arch::X64),
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
pub enum BackendChoice {
    Auto,
    WindowsDism,
    CrossWimlib,
}

#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    WindowsDism,
    CrossWimlib,
}

impl BackendKind {
    pub fn resolve(ctx: &crate::deps::DepContext, choice: BackendChoice) -> anyhow::Result<Self> {
        use anyhow::anyhow;
        match choice {
            BackendChoice::WindowsDism => {
                if cfg!(windows) && ctx.dism.is_some() {
                    Ok(BackendKind::WindowsDism)
                } else {
                    Err(anyhow!(
                        "Backend windows-dism requested, but DISM was not detected (or not running on Windows)"
                    ))
                }
            }
            BackendChoice::CrossWimlib => {
                if ctx.wimlib_imagex.is_some() && ctx.hivexregedit.is_some() {
                    Ok(BackendKind::CrossWimlib)
                } else {
                    Err(anyhow!(
                        "Backend cross-wimlib requested, but wimlib-imagex/hivexregedit were not detected"
                    ))
                }
            }
            BackendChoice::Auto => {
                if cfg!(windows) && ctx.dism.is_some() && ctx.reg.is_some() {
                    return Ok(BackendKind::WindowsDism);
                }
                if ctx.wimlib_imagex.is_some() && ctx.hivexregedit.is_some() {
                    return Ok(BackendKind::CrossWimlib);
                }
                Err(anyhow!(
                    "Unable to auto-select backend: need either Windows DISM+reg.exe, or wimlib-imagex+hivexregedit"
                ))
            }
        }
    }
}

