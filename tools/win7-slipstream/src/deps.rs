use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DepContext {
    pub seven_zip: Option<PathBuf>,
    pub xorriso: Option<PathBuf>,
    pub oscdimg: Option<PathBuf>,
    pub dism: Option<PathBuf>,
    pub bcdedit: Option<PathBuf>,
    pub reg: Option<PathBuf>,
    pub powershell: Option<PathBuf>,
    pub wimlib_imagex: Option<PathBuf>,
    pub hivexregedit: Option<PathBuf>,
}

impl DepContext {
    pub fn detect() -> Self {
        Self {
            seven_zip: find_any(&["7z", "7z.exe", "7za", "7za.exe"]),
            xorriso: find_any(&["xorriso", "xorriso.exe"]),
            oscdimg: find_any(&["oscdimg", "oscdimg.exe"]),
            dism: find_any(&["dism", "dism.exe"]),
            bcdedit: find_any(&["bcdedit", "bcdedit.exe"]),
            reg: find_any(&["reg", "reg.exe"]),
            powershell: find_any(&["powershell", "powershell.exe", "pwsh", "pwsh.exe"]),
            wimlib_imagex: find_any(&["wimlib-imagex", "wimlib-imagex.exe"]),
            hivexregedit: find_any(&["hivexregedit", "hivexregedit.exe"]),
        }
    }
}

fn find_any(candidates: &[&str]) -> Option<PathBuf> {
    for cand in candidates {
        if let Ok(path) = which::which(cand) {
            return Some(path);
        }
    }
    None
}

pub fn print_deps(ctx: &DepContext) {
    fn line(name: &str, found: &Option<PathBuf>, hints: &[&str]) {
        if let Some(path) = found {
            println!("{name}: FOUND ({})", path.display());
        } else {
            println!("{name}: MISSING");
            for hint in hints {
                println!("  - {hint}");
            }
        }
    }

    println!("aero-win7-slipstream external dependencies:");
    println!();

    line(
        "7z (extract ISO)",
        &ctx.seven_zip,
        &[
            "Windows: winget install 7zip.7zip",
            "Linux:   sudo apt-get install p7zip-full",
            "macOS:   brew install p7zip",
        ],
    );
    line(
        "xorriso (extract/rebuild ISO)",
        &ctx.xorriso,
        &[
            "Linux:   sudo apt-get install xorriso",
            "macOS:   brew install xorriso",
            "Windows: choco install xorriso (or install via MSYS2)",
        ],
    );
    line(
        "oscdimg (Windows ADK, rebuild ISO)",
        &ctx.oscdimg,
        &[
            "Windows: Install Windows ADK \"Deployment Tools\" to get oscdimg.exe",
            "Fallback: install xorriso and use --backend cross-wimlib",
        ],
    );
    line(
        "DISM (mount/patch WIM on Windows)",
        &ctx.dism,
        &["Windows: DISM is built-in on modern Windows; ensure it's in PATH"],
    );
    line(
        "bcdedit (inspect/verify BCD stores on Windows)",
        &ctx.bcdedit,
        &["Windows: bcdedit is built-in; ensure it's in PATH"],
    );
    line(
        "reg.exe (offline registry edits on Windows)",
        &ctx.reg,
        &["Windows: reg.exe is built-in; ensure it's in PATH"],
    );
    line(
        "PowerShell (fallback ISO extraction on Windows)",
        &ctx.powershell,
        &["Windows: PowerShell is built-in; ensure powershell.exe is in PATH"],
    );
    line(
        "wimlib-imagex (mount/patch WIM cross-platform)",
        &ctx.wimlib_imagex,
        &[
            "Linux: sudo apt-get install wimtools (or wimlib-tools)",
            "macOS: brew install wimlib",
        ],
    );
    line(
        "hivexregedit (offline hive edits cross-platform)",
        &ctx.hivexregedit,
        &[
            "Linux: sudo apt-get install libhivex-bin",
            "macOS: brew install hivex (or libguestfs)",
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use tempfile::TempDir;

    #[test]
    fn detects_fake_executable_via_path() {
        let dir = TempDir::new().unwrap();
        let exe_name = if cfg!(windows) { "7z.exe" } else { "7z" };
        let exe_path = dir.path().join(exe_name);
        std::fs::write(&exe_path, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&exe_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&exe_path, perms).unwrap();
        }

        let original_path = env::var_os("PATH").unwrap_or_default();
        let mut new_path = dir.path().as_os_str().to_os_string();
        new_path.push(if cfg!(windows) { ";" } else { ":" });
        new_path.push(&original_path);

        env::set_var("PATH", &new_path);
        let ctx = DepContext::detect();
        env::set_var("PATH", original_path);

        assert!(ctx.seven_zip.is_some());
    }
}
