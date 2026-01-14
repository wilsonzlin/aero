use std::env;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

// Preferred regeneration command (covers all deterministic in-repo fixtures).
const REGEN_CMD: &str = "cargo xtask fixtures";
const CHECK_CMD: &str = "cargo xtask fixtures --check";

// Preferred regeneration command for the BIOS ROM fixture only.
const REGEN_CMD_BIOS_ROM: &str = "cargo xtask bios-rom";
const CHECK_CMD_BIOS_ROM: &str = "cargo xtask bios-rom --check";

// Alternative convenience for regenerating/checking just the BIOS ROM fixture.
const REGEN_CMD_ALT: &str = "cargo run -p firmware --bin gen_bios_rom --locked";
// When invoking `--check` via `cargo run`, the `--` separator is required so Cargo forwards the
// flag to the binary rather than trying to parse it itself.
const CHECK_CMD_ALT: &str = "cargo run -p firmware --bin gen_bios_rom --locked -- --check";

const BIOS_ROM_LEN: usize = 0x10000; // 64KiB

fn repo_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` for this binary is `<repo>/crates/firmware`.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn assets_bios_bin_path() -> PathBuf {
    repo_root().join("assets").join("bios.bin")
}

fn usage(bin_name: &str) -> String {
    format!(
        "Usage: {bin_name} [--check]\n\
\n\
Generate the canonical 64KiB BIOS ROM image via `firmware::bios::build_bios_rom()` and write it\n\
to `<repo>/assets/bios.bin`.\n\
\n\
Preferred (regen/check all deterministic fixtures):\n\
    {REGEN_CMD}\n\
    {CHECK_CMD}\n\
\n\
Preferred (regen/check BIOS ROM only):\n\
    {REGEN_CMD_BIOS_ROM}\n\
    {CHECK_CMD_BIOS_ROM}\n\
\n\
This binary is an alternative convenience for regenerating just the BIOS ROM fixture:\n\
    {REGEN_CMD_ALT}\n\
    {CHECK_CMD_ALT}\n\
\n\
Options:\n\
    --check   Verify `<repo>/assets/bios.bin` matches the generator output and exit non-zero if it differs.\n"
    )
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;

    let tmp_path = path.with_extension(format!("bin.tmp.{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }

    match fs::rename(&tmp_path, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Windows refuses to replace an existing file with `rename`. Retry with an explicit
            // delete (still an atomic rename once the destination is absent).
            if cfg!(windows) && path.exists() {
                fs::remove_file(path)?;
                fs::rename(&tmp_path, path)?;
                Ok(())
            } else {
                let _ = fs::remove_file(&tmp_path);
                Err(err)
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut check = false;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--check" => check = true,
            "--help" | "-h" => {
                eprintln!("{}", usage(&env::args().next().unwrap_or_else(|| "gen_bios_rom".into())));
                return Ok(());
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "unknown argument: {other}\n\n{}",
                        usage(&env::args().next().unwrap_or_else(|| "gen_bios_rom".into()))
                    ),
                )
                .into());
            }
        }
    }

    let rom = firmware::bios::build_bios_rom();
    if rom.len() != BIOS_ROM_LEN {
        return Err(io::Error::other(
            format!(
                "generated BIOS ROM has unexpected size: {} bytes (expected {BIOS_ROM_LEN} bytes)",
                rom.len()
            ),
        )
        .into());
    }
    if rom.len() > 1024 * 1024 {
        return Err(io::Error::other(
            format!(
                "generated BIOS ROM is too large for the repo allowlist ({} bytes > 1 MiB)",
                rom.len()
            ),
        )
        .into());
    }

    let out_path = assets_bios_bin_path();
    if check {
        match fs::read(&out_path) {
            Ok(existing) if existing == rom => return Ok(()),
            Ok(existing) => {
                return Err(io::Error::other(
                    format!(
                        "{} does not match the canonical generator output ({} bytes vs {} bytes).\n\
Regenerate with: {REGEN_CMD_BIOS_ROM} (or: {REGEN_CMD}, or: {REGEN_CMD_ALT})",
                        out_path.display(),
                        existing.len(),
                        rom.len()
                    ),
                )
                .into());
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "{} does not exist.\nRegenerate with: {REGEN_CMD_BIOS_ROM} (or: {REGEN_CMD}, or: {REGEN_CMD_ALT})",
                        out_path.display()
                    ),
                )
                .into());
            }
            Err(err) => return Err(err.into()),
        }
    }

    atomic_write(&out_path, &rom)?;
    Ok(())
}
