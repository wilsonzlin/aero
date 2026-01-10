use crate::bcd;
use crate::cli::{ArchChoice, BackendKind};
use crate::deps::DepContext;
use crate::hash;
use crate::manifest::{CertificateManifest, PatchedPath};
use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use sha1::{Digest as _, Sha1};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use walkdir::WalkDir;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Arch {
    X86,
    X64,
}

impl Arch {
    pub fn iso_dir_name(self) -> &'static str {
        match self {
            Arch::X86 => "x86",
            Arch::X64 => "amd64",
        }
    }

    pub fn unattend_processor_arch(self) -> &'static str {
        match self {
            Arch::X86 => "x86",
            Arch::X64 => "amd64",
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum, Serialize, Deserialize)]
pub enum SigningMode {
    #[value(name = "none")]
    #[serde(rename = "none")]
    None,
    // Match the CLI/user-story spelling ("testsigning") while still accepting the more common
    // hyphenated form as an alias.
    #[value(name = "testsigning", alias = "test-signing")]
    #[serde(rename = "testsigning")]
    TestSigning,
    #[value(name = "nointegritychecks", alias = "no-integrity-checks")]
    #[serde(rename = "nointegritychecks")]
    NoIntegrityChecks,
}

pub fn resolve_arch(iso_root: &Path, choice: &ArchChoice) -> Result<Arch> {
    if let Some(arch) = choice.to_arch() {
        return Ok(arch);
    }

    // Heuristic: Windows 7 x64 installation media typically includes an EFI folder (UEFI boot),
    // while x86 usually does not.
    let uefi_bcd = iso_root
        .join("efi")
        .join("microsoft")
        .join("boot")
        .join("BCD");
    let uefi_bootmgfw = iso_root
        .join("efi")
        .join("microsoft")
        .join("boot")
        .join("bootmgfw.efi");
    if uefi_bcd.exists() || uefi_bootmgfw.exists() {
        return Ok(Arch::X64);
    }
    Ok(Arch::X86)
}

pub fn select_driver_dir(drivers_root: &Path, arch: Arch) -> Result<PathBuf> {
    let candidates: &[&str] = match arch {
        Arch::X86 => &["x86", "i386", "x32", "32"],
        Arch::X64 => &["amd64", "x64", "x86_64", "64"],
    };

    for cand in candidates {
        let p = drivers_root.join(cand);
        if p.is_dir() {
            return Ok(p);
        }
    }

    Err(anyhow!(
        "Unable to locate driver pack subdirectory for {arch:?} under {} (expected one of: {})",
        drivers_root.display(),
        candidates.join(", ")
    ))
}

pub fn copy_dir_recursive(
    src: &Path,
    dest_abs: &Path,
    dest_rel: &Path,
    patched_paths: &mut Vec<PatchedPath>,
) -> Result<()> {
    fs::create_dir_all(dest_abs)
        .with_context(|| format!("Failed to create {}", dest_abs.display()))?;
    for entry in WalkDir::new(src).follow_links(false) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src).unwrap();
        let dst = dest_abs.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&dst)?;
            continue;
        }
        if entry.file_type().is_file() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &dst).with_context(|| {
                format!(
                    "Failed to copy {} to {}",
                    entry.path().display(),
                    dst.display()
                )
            })?;
            let iso_path = dest_rel
                .join(rel)
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            patched_paths.push(PatchedPath::new_file(iso_path));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct CertInfo {
    pub der: Vec<u8>,
    pub sha256: String,
    pub thumbprint_sha1: String,
}

impl CertInfo {
    pub fn from_path(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("Failed to read cert: {}", path.display()))?;
        let der = if bytes
            .windows(b"-----BEGIN CERTIFICATE-----".len())
            .any(|w| w == b"-----BEGIN CERTIFICATE-----")
        {
            pem_to_der(&bytes).context("Failed to parse PEM certificate")?
        } else {
            bytes
        };

        let sha256 = hash::sha256_bytes(&der);
        let mut sha1 = Sha1::new();
        sha1.update(&der);
        let thumbprint_sha1 = hex::encode_upper(sha1.finalize());

        Ok(Self {
            der,
            sha256,
            thumbprint_sha1,
        })
    }

    pub fn as_manifest(&self) -> CertificateManifest {
        CertificateManifest {
            sha256: self.sha256.clone(),
            thumbprint_sha1: self.thumbprint_sha1.clone(),
        }
    }

    pub fn to_reg_patch(&self, root_prefix: &str) -> String {
        let blob = format_reg_binary(&self.der);
        let thumb = &self.thumbprint_sha1;
        format!(
            "Windows Registry Editor Version 5.00\n\n\
[{}\\Microsoft\\SystemCertificates\\ROOT\\Certificates\\{}]\n\
\"Blob\"=hex:{}\n\n\
[{}\\Microsoft\\SystemCertificates\\TrustedPublisher\\Certificates\\{}]\n\
\"Blob\"=hex:{}\n",
            root_prefix, thumb, blob, root_prefix, thumb, blob
        )
    }
}

fn pem_to_der(bytes: &[u8]) -> Result<Vec<u8>> {
    let text = String::from_utf8_lossy(bytes);
    let mut in_body = false;
    let mut b64 = String::new();
    for line in text.lines() {
        if line.contains("BEGIN CERTIFICATE") {
            in_body = true;
            continue;
        }
        if line.contains("END CERTIFICATE") {
            break;
        }
        if in_body {
            b64.push_str(line.trim());
        }
    }
    if b64.is_empty() {
        return Err(anyhow!("PEM file did not contain a certificate body"));
    }
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    STANDARD.decode(b64).context("Base64 decode failed")
}

pub fn format_reg_binary(bytes: &[u8]) -> String {
    // Wrap at 16 bytes per line for readability and to avoid line-length limits in reg.exe.
    let mut out = String::new();
    for (i, byte) in bytes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if i > 0 && i % 16 == 0 {
            out.push_str("\\\n  ");
        }
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

pub struct Backend {
    kind: BackendKind,
    deps: DepContext,
    verbose: bool,
    workdir: PathBuf,
}

impl Backend {
    pub fn new_with_workdir(kind: BackendKind, deps: &DepContext, workdir: &Path, verbose: bool) -> Result<Self> {
        match kind {
            BackendKind::WindowsDism => {
                if !cfg!(windows) {
                    return Err(anyhow!("windows-dism backend requires Windows"));
                }
                if deps.dism.is_none() || deps.reg.is_none() {
                    return Err(anyhow!(
                        "windows-dism backend requires dism and reg.exe in PATH"
                    ));
                }
                if deps.bcdedit.is_none() {
                    return Err(anyhow!(
                        "windows-dism backend requires bcdedit in PATH"
                    ));
                }
            }
            BackendKind::CrossWimlib => {
                if deps.wimlib_imagex.is_none() || deps.hivexregedit.is_none() {
                    return Err(anyhow!(
                        "cross-wimlib backend requires wimlib-imagex and hivexregedit in PATH"
                    ));
                }
            }
        }

        fs::create_dir_all(workdir).with_context(|| {
            format!(
                "Failed to create backend workdir at {}",
                workdir.display()
            )
        })?;

        Ok(Self {
            kind,
            deps: deps.clone(),
            verbose,
            workdir: workdir.to_path_buf(),
        })
    }

    pub fn supports_offline_driver_injection(&self) -> bool {
        matches!(self.kind, BackendKind::WindowsDism)
    }

    pub fn patch_bcd_store(&self, store: &Path, mode: SigningMode) -> Result<()> {
        if let Some(bcdedit) = self.deps.bcdedit.as_deref() {
            return bcd::patch_with_bcdedit(bcdedit, store, mode, self.verbose);
        }
        let hivex = self.deps.hivexregedit.as_deref().ok_or_else(|| {
            anyhow!(
                "No BCD patcher available: need bcdedit (Windows) or hivexregedit (cross)"
            )
        })?;
        bcd::patch_with_hivex(hivex, store, mode, self.verbose)
    }

    pub fn wim_indexes(&self, wim: &Path) -> Result<Vec<u32>> {
        match self.kind {
            BackendKind::WindowsDism => self.wim_indexes_dism(wim),
            BackendKind::CrossWimlib => self.wim_indexes_wimlib(wim),
        }
    }

    pub fn patch_install_wim_bcd_template(
        &self,
        install_wim: &Path,
        indexes: &[u32],
        mode: SigningMode,
    ) -> Result<()> {
        for idx in indexes {
            self.with_mounted_wim(install_wim, *idx, |mount| {
                let bcd_template = mount
                    .join("Windows")
                    .join("System32")
                    .join("config")
                    .join("BCD-Template");
                if !bcd_template.is_file() {
                    return Err(anyhow!(
                        "BCD-Template not found in image index {idx} at {}",
                        bcd_template.display()
                    ));
                }
                self.patch_bcd_store(&bcd_template, mode)
                    .context("Failed to patch BCD-Template")?;
                Ok(())
            })?;
        }
        Ok(())
    }

    pub fn inject_cert_into_wim(&self, wim: &Path, indexes: &[u32], cert: &CertInfo) -> Result<()> {
        for idx in indexes {
            self.with_mounted_wim(wim, *idx, |mount| {
                let hive = mount
                    .join("Windows")
                    .join("System32")
                    .join("config")
                    .join("SOFTWARE");
                if !hive.is_file() {
                    return Err(anyhow!(
                        "SOFTWARE hive not found in image index {idx} at {}",
                        hive.display()
                    ));
                }
                match self.kind {
                    BackendKind::WindowsDism => self.inject_cert_windows(&hive, cert, idx),
                    BackendKind::CrossWimlib => self.inject_cert_hivex(&hive, cert),
                }
            })?;
        }
        Ok(())
    }

    pub fn inject_drivers_into_wim(
        &self,
        wim: &Path,
        indexes: &[u32],
        drivers: &Path,
        mode: SigningMode,
    ) -> Result<()> {
        if !self.supports_offline_driver_injection() {
            return Err(anyhow!(
                "Offline driver injection is not supported by backend {:?}",
                self.kind
            ));
        }

        for idx in indexes {
            self.with_mounted_wim(wim, *idx, |mount| {
                self.inject_drivers_windows(mount, drivers, mode)
                    .with_context(|| format!("Failed to inject drivers into index {idx}"))?;
                Ok(())
            })?;
        }
        Ok(())
    }

    fn wim_indexes_dism(&self, wim: &Path) -> Result<Vec<u32>> {
        let dism = self.deps.dism.as_deref().unwrap();
        let out = run_capture(
            Command::new(dism)
                .arg("/English")
                .arg("/Get-WimInfo")
                .arg(format!("/WimFile:{}", wim.display())),
            self.verbose,
        )
        .context("Failed to run DISM /Get-WimInfo")?;

        parse_wim_indexes(&out, "Index :")
    }

    fn wim_indexes_wimlib(&self, wim: &Path) -> Result<Vec<u32>> {
        let wimlib = self.deps.wimlib_imagex.as_deref().unwrap();
        let out = run_capture(Command::new(wimlib).arg("info").arg(wim), self.verbose)
            .context("Failed to run wimlib-imagex info")?;
        parse_wim_indexes(&out, "Index:")
    }

    fn with_mounted_wim<F>(&self, wim: &Path, index: u32, f: F) -> Result<()>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        self.with_mounted_wim_impl(wim, index, true, f)
    }

    fn with_mounted_wim_readonly<F>(&self, wim: &Path, index: u32, f: F) -> Result<()>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        self.with_mounted_wim_impl(wim, index, false, f)
    }

    fn with_mounted_wim_impl<F>(&self, wim: &Path, index: u32, writable: bool, f: F) -> Result<()>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        let mount_base = self.workdir.join(if writable { "mount" } else { "mount-ro" });
        fs::create_dir_all(&mount_base).context("Failed to create mount base")?;

        let wim_name = wim
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("image");
        let mount_dir = mount_base.join(format!("{wim_name}-{index}"));
        if mount_dir.exists() {
            fs::remove_dir_all(&mount_dir).with_context(|| {
                format!("Failed to clear mount dir {}", mount_dir.display())
            })?;
        }
        fs::create_dir_all(&mount_dir).with_context(|| {
            format!("Failed to create mount dir {}", mount_dir.display())
        })?;

        match self.kind {
            BackendKind::WindowsDism => {
                let dism = self.deps.dism.as_deref().unwrap();
                let mut mount_cmd = Command::new(dism);
                mount_cmd
                    .arg("/English")
                    .arg("/Mount-Wim")
                    .arg(format!("/WimFile:{}", wim.display()))
                    .arg(format!("/Index:{index}"))
                    .arg(format!("/MountDir:{}", mount_dir.display()));
                if !writable {
                    mount_cmd.arg("/ReadOnly");
                }
                run(&mut mount_cmd, self.verbose)
                .context("DISM mount failed")?;

                let res = f(&mount_dir);

                let mut unmount = Command::new(dism);
                unmount
                    .arg("/English")
                    .arg("/Unmount-Wim")
                    .arg(format!("/MountDir:{}", mount_dir.display()))
                    .arg(if writable { "/Commit" } else { "/Discard" });
                let unmount_res = run(&mut unmount, self.verbose).context("DISM unmount failed");

                res.and(unmount_res)
            }
            BackendKind::CrossWimlib => {
                let wimlib = self.deps.wimlib_imagex.as_deref().unwrap();
                let mut mount_cmd = Command::new(wimlib);
                mount_cmd
                    .arg("mount")
                    .arg(wim)
                    .arg(index.to_string())
                    .arg(&mount_dir);
                if writable {
                    mount_cmd.arg("--rw");
                }
                run(&mut mount_cmd, self.verbose)
                .context("wimlib-imagex mount failed")?;

                let res = f(&mount_dir);

                let unmount_res = run(
                    Command::new(wimlib)
                        .arg("unmount")
                        .arg(&mount_dir)
                        .arg(if writable { "--commit" } else { "--discard" }),
                    self.verbose,
                )
                .context("wimlib-imagex unmount failed");

                res.and(unmount_res)
            }
        }
    }

    fn inject_cert_windows(&self, software_hive: &Path, cert: &CertInfo, idx: &u32) -> Result<()> {
        let reg = self.deps.reg.as_deref().unwrap();
        let key_name = format!("HKLM\\AERO_OFFSOFTWARE_{}", idx);
        run(
            Command::new(reg)
                .arg("load")
                .arg(&key_name)
                .arg(software_hive),
            self.verbose,
        )
        .context("reg load failed")?;

        struct UnloadGuard {
            reg: PathBuf,
            key: String,
            verbose: bool,
        }
        impl Drop for UnloadGuard {
            fn drop(&mut self) {
                let _ = run(
                    Command::new(&self.reg).arg("unload").arg(&self.key),
                    self.verbose,
                );
            }
        }
        let _guard = UnloadGuard {
            reg: reg.to_path_buf(),
            key: key_name.clone(),
            verbose: self.verbose,
        };

        let reg_patch = cert.to_reg_patch(&format!("HKEY_LOCAL_MACHINE\\{}", key_name.trim_start_matches("HKLM\\")));
        let patch_file = tempfile::Builder::new()
            .prefix("aero-win7-slipstream-cert-")
            .suffix(".reg")
            .tempfile_in(&self.workdir)
            .context("Failed to create temporary cert patch file")?;
        fs::write(patch_file.path(), reg_patch).context("Failed to write cert patch file")?;

        run(
            Command::new(reg)
                .arg("import")
                .arg(patch_file.path()),
            self.verbose,
        )
        .context("reg import failed")?;

        Ok(())
    }

    fn inject_cert_hivex(&self, software_hive: &Path, cert: &CertInfo) -> Result<()> {
        let hivex = self.deps.hivexregedit.as_deref().unwrap();
        let reg_patch = cert.to_reg_patch("HKEY_LOCAL_MACHINE");
        let patch_file = tempfile::Builder::new()
            .prefix("aero-win7-slipstream-cert-")
            .suffix(".reg")
            .tempfile_in(&self.workdir)
            .context("Failed to create temporary cert patch file")?;
        fs::write(patch_file.path(), reg_patch).context("Failed to write cert patch file")?;
        run(
            Command::new(hivex)
                .arg("--merge")
                .arg(software_hive)
                .arg(patch_file.path()),
            self.verbose,
        )
        .context("hivexregedit merge failed")?;
        Ok(())
    }

    fn inject_drivers_windows(&self, mount_dir: &Path, drivers: &Path, mode: SigningMode) -> Result<()> {
        let dism = self.deps.dism.as_deref().unwrap();
        let mut cmd = Command::new(dism);
        cmd.arg("/English")
            .arg(format!("/Image:{}", mount_dir.display()))
            .arg("/Add-Driver")
            .arg(format!("/Driver:{}", drivers.display()))
            .arg("/Recurse");
        if mode != SigningMode::None {
            cmd.arg("/ForceUnsigned");
        }
        run(&mut cmd, self.verbose).context("DISM /Add-Driver failed")?;
        Ok(())
    }

    pub fn verify_bcd_template_in_wim(
        &self,
        install_wim: &Path,
        indexes: &[u32],
        mode: SigningMode,
    ) -> Result<()> {
        for idx in indexes {
            self.with_mounted_wim_readonly(install_wim, *idx, |mount| {
                let bcd_template = mount
                    .join("Windows")
                    .join("System32")
                    .join("config")
                    .join("BCD-Template");
                if !bcd_template.is_file() {
                    return Err(anyhow!(
                        "BCD-Template not found in image index {idx} at {}",
                        bcd_template.display()
                    ));
                }
                verify_bcd_hive(&self.deps, &bcd_template, mode, self.verbose).with_context(|| {
                    format!("BCD-Template policy verification failed for index {idx}")
                })?;
                Ok(())
            })?;
        }
        Ok(())
    }

    pub fn verify_cert_in_wim(&self, wim: &Path, indexes: &[u32], thumbprint_sha1: &str) -> Result<()> {
        for idx in indexes {
            self.with_mounted_wim_readonly(wim, *idx, |mount| {
                let hive = mount
                    .join("Windows")
                    .join("System32")
                    .join("config")
                    .join("SOFTWARE");
                if !hive.is_file() {
                    return Err(anyhow!(
                        "SOFTWARE hive not found in image index {idx} at {}",
                        hive.display()
                    ));
                }
                match self.kind {
                    BackendKind::WindowsDism => verify_cert_windows(&self.deps, &hive, thumbprint_sha1, idx, self.verbose),
                    BackendKind::CrossWimlib => verify_cert_hivex(&self.deps, &hive, thumbprint_sha1, self.verbose),
                }
            })?;
        }
        Ok(())
    }
}

fn verify_bcd_hive(deps: &DepContext, hive: &Path, mode: SigningMode, verbose: bool) -> Result<()> {
    if mode == SigningMode::None {
        return Ok(());
    }

    if let Some(bcdedit) = deps.bcdedit.as_deref() {
        let out = run_capture(
            Command::new(bcdedit)
                .arg("/store")
                .arg(hive)
                .arg("/enum")
                .arg("{default}"),
            verbose,
        )
        .context("bcdedit failed")?;
        let out_lc = out.to_lowercase();
        match mode {
            SigningMode::TestSigning => {
                if out_lc.contains("testsigning") && (out_lc.contains("yes") || out_lc.contains("on")) {
                    return Ok(());
                }
                return Err(anyhow!("bcdedit output did not show testsigning enabled"));
            }
            SigningMode::NoIntegrityChecks => {
                if out_lc.contains("nointegritychecks") && (out_lc.contains("yes") || out_lc.contains("on")) {
                    return Ok(());
                }
                return Err(anyhow!(
                    "bcdedit output did not show nointegritychecks enabled"
                ));
            }
            SigningMode::None => {}
        }
    }

    let hivex = deps.hivexregedit.as_deref().ok_or_else(|| {
        anyhow!("Need hivexregedit or bcdedit to verify BCD hives")
    })?;
    let exported = run_capture(Command::new(hivex).arg("--export").arg(hive), verbose)
        .context("hivexregedit export failed")?;
    if !bcd::hive_contains_policy(&exported, mode) {
        return Err(anyhow!(
            "BCD hive export did not contain expected policy flag for mode {:?}",
            mode
        ));
    }
    Ok(())
}

fn verify_cert_hivex(deps: &DepContext, software_hive: &Path, thumbprint_sha1: &str, verbose: bool) -> Result<()> {
    let hivex = deps.hivexregedit.as_deref().ok_or_else(|| anyhow!("Need hivexregedit"))?;
    let exported = run_capture(Command::new(hivex).arg("--export").arg(software_hive), verbose)
        .context("hivexregedit export failed")?;

    let root_key = format!(
        "\\Microsoft\\SystemCertificates\\ROOT\\Certificates\\{}]",
        thumbprint_sha1
    );
    let tp_key = format!(
        "\\Microsoft\\SystemCertificates\\TrustedPublisher\\Certificates\\{}]",
        thumbprint_sha1
    );

    if !exported.contains(&root_key) {
        return Err(anyhow!(
            "SOFTWARE hive does not contain ROOT certificate entry for {}",
            thumbprint_sha1
        ));
    }
    if !exported.contains(&tp_key) {
        return Err(anyhow!(
            "SOFTWARE hive does not contain TrustedPublisher certificate entry for {}",
            thumbprint_sha1
        ));
    }
    Ok(())
}

fn verify_cert_windows(
    deps: &DepContext,
    software_hive: &Path,
    thumbprint_sha1: &str,
    idx: &u32,
    verbose: bool,
) -> Result<()> {
    let reg = deps.reg.as_deref().ok_or_else(|| anyhow!("Need reg.exe"))?;
    let key_name = format!("HKLM\\AERO_VERIFY_SOFTWARE_{}", idx);
    run(
        Command::new(reg)
            .arg("load")
            .arg(&key_name)
            .arg(software_hive),
        verbose,
    )
    .context("reg load failed")?;

    struct UnloadGuard {
        reg: PathBuf,
        key: String,
        verbose: bool,
    }
    impl Drop for UnloadGuard {
        fn drop(&mut self) {
            let _ = run(Command::new(&self.reg).arg("unload").arg(&self.key), self.verbose);
        }
    }
    let _guard = UnloadGuard {
        reg: reg.to_path_buf(),
        key: key_name.clone(),
        verbose,
    };

    let root_key = format!(
        "{}\\Microsoft\\SystemCertificates\\ROOT\\Certificates\\{}",
        key_name, thumbprint_sha1
    );
    let tp_key = format!(
        "{}\\Microsoft\\SystemCertificates\\TrustedPublisher\\Certificates\\{}",
        key_name, thumbprint_sha1
    );

    reg_query_key(reg, &root_key, verbose).context("ROOT cert key missing")?;
    reg_query_key(reg, &tp_key, verbose).context("TrustedPublisher cert key missing")?;
    Ok(())
}

fn reg_query_key(reg: &Path, key: &str, verbose: bool) -> Result<()> {
    let mut cmd = Command::new(reg);
    cmd.arg("query").arg(key);
    if verbose {
        eprintln!("> {:?}", cmd);
    }
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to spawn reg query")?;
    if !status.success() {
        return Err(anyhow!("reg query failed for {key}"));
    }
    Ok(())
}

fn parse_wim_indexes(output: &str, prefix: &str) -> Result<Vec<u32>> {
    let mut out = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(prefix) {
            let num = rest.trim().parse::<u32>();
            if let Ok(idx) = num {
                out.push(idx);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    if out.is_empty() {
        return Err(anyhow!(
            "Failed to parse WIM indexes from tool output (looked for prefix {prefix:?})"
        ));
    }
    Ok(out)
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
        return Err(anyhow!("External command failed with status: {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reg_binary_wrapping_is_stable() {
        let bytes = (0u8..40).collect::<Vec<_>>();
        let s = format_reg_binary(&bytes);
        assert!(s.contains("\\\n"));
        assert!(s.starts_with("00,01,02"));
    }
}
