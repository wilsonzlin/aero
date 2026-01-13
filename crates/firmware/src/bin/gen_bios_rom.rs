use std::env;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

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
    if rom.len() > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
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
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "{} does not match the canonical generator output ({} bytes vs {} bytes).\n\
Regenerate with: cargo run -p firmware --bin gen_bios_rom",
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
                        "{} does not exist.\nRegenerate with: cargo run -p firmware --bin gen_bios_rom",
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

