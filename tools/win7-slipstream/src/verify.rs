use crate::bcd;
use crate::cli::VerifyIsoArgs;
use crate::deps::DepContext;
use crate::iso::IsoExtractor;
use crate::manifest::Manifest;
use crate::wim::{Backend, SigningMode};
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn run_verify(ctx: &DepContext, args: VerifyIsoArgs) -> Result<()> {
    let input = args
        .input
        .canonicalize()
        .with_context(|| format!("Failed to resolve input path: {}", args.input.display()))?;

    let (iso_root, _tmp) = if input.is_dir() {
        (input, None)
    } else if input.is_file() {
        let tmp = tempfile::TempDir::new().context("Failed to create temporary extraction dir")?;
        let iso_root = tmp.path().join("iso-root");
        fs::create_dir_all(&iso_root)?;
        let extractor = IsoExtractor::detect(ctx)?;
        extractor.extract(&input, &iso_root, args.verbose)?;
        (iso_root, Some(tmp))
    } else {
        return Err(anyhow!(
            "Input path is neither a file nor a directory: {}",
            input.display()
        ));
    };

    verify_extracted_tree(ctx, &iso_root, args.verbose)
}

fn verify_extracted_tree(ctx: &DepContext, iso_root: &Path, verbose: bool) -> Result<()> {
    let manifest_path = iso_root.join("AERO").join("MANIFEST.json");
    let manifest_bytes = fs::read(&manifest_path).with_context(|| {
        format!(
            "Missing manifest at {} (not an Aero-patched ISO?)",
            manifest_path.display()
        )
    })?;
    let manifest = Manifest::from_json(&manifest_bytes)?;

    let drivers_dir = iso_root
        .join("AERO")
        .join("DRIVERS")
        .join(manifest.arch.iso_dir_name());
    if !drivers_dir.is_dir() {
        return Err(anyhow!(
            "Missing injected drivers directory at {}",
            drivers_dir.display()
        ));
    }

    if manifest.unattend != crate::unattend::UnattendMode::None {
        let unattend_path = iso_root.join("autounattend.xml");
        if !unattend_path.is_file() {
            return Err(anyhow!(
                "Manifest says unattend mode {:?} but autounattend.xml is missing at {}",
                manifest.unattend,
                unattend_path.display()
            ));
        }
        let xml = fs::read_to_string(&unattend_path)
            .with_context(|| format!("Failed to read {}", unattend_path.display()))?;
        let expected_fragment = format!(
            "%configsetroot%\\\\AERO\\\\DRIVERS\\\\{}",
            manifest.arch.iso_dir_name()
        );
        if !xml.contains(&expected_fragment) {
            return Err(anyhow!(
                "autounattend.xml does not contain expected driver path fragment: {expected_fragment}"
            ));
        }
    }

    let bcd_paths = [
        iso_root.join("boot").join("BCD"),
        iso_root
            .join("efi")
            .join("microsoft")
            .join("boot")
            .join("BCD"),
    ];
    for store in bcd_paths.iter().filter(|p| p.is_file()) {
        verify_bcd_store(ctx, store, manifest.signing_mode, verbose)
            .with_context(|| format!("BCD store policy check failed for {}", store.display()))?;
    }

    let backend_kind = if cfg!(windows) && ctx.dism.is_some() && ctx.reg.is_some() && ctx.bcdedit.is_some() {
        crate::cli::BackendKind::WindowsDism
    } else if ctx.wimlib_imagex.is_some() && ctx.hivexregedit.is_some() {
        crate::cli::BackendKind::CrossWimlib
    } else {
        return Err(anyhow!(
            "Cannot verify WIM contents: need either DISM+reg.exe+bcdedit (Windows) or wimlib-imagex+hivexregedit"
        ));
    };
    let backend_workdir = tempfile::TempDir::new().context("Failed to create verify workdir")?;
    let backend = Backend::new_with_workdir(backend_kind, ctx, backend_workdir.path(), verbose)?;

    let sources = iso_root.join("sources");
    let boot_wim = sources.join("boot.wim");
    if boot_wim.is_file() {
        if manifest.signing_mode == SigningMode::TestSigning {
            if let Some(cert) = manifest.certificate.as_ref() {
                backend
                    .verify_cert_in_wim(&boot_wim, &[2], &cert.thumbprint_sha1)
                    .context("boot.wim certificate verification failed")?;
            }
        }
    }

    let install_wim = sources.join("install.wim");
    if install_wim.is_file() {
        let indexes = backend.wim_indexes(&install_wim)?;
        backend
            .verify_bcd_template_in_wim(&install_wim, &indexes, manifest.signing_mode)
            .context("install.wim BCD-Template verification failed")?;
        if manifest.signing_mode == SigningMode::TestSigning {
            if let Some(cert) = manifest.certificate.as_ref() {
                backend
                    .verify_cert_in_wim(&install_wim, &indexes, &cert.thumbprint_sha1)
                    .context("install.wim certificate verification failed")?;
            }
        }
    }

    println!("OK: {}", iso_root.display());
    Ok(())
}

fn verify_bcd_store(ctx: &DepContext, store: &Path, mode: SigningMode, verbose: bool) -> Result<()> {
    if mode == SigningMode::None {
        return Ok(());
    }

    if let Some(bcdedit) = ctx.bcdedit.as_deref() {
        let out = run_capture(
            Command::new(bcdedit)
                .arg("/store")
                .arg(store)
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

    let hivex = ctx.hivexregedit.as_deref().ok_or_else(|| {
        anyhow!(
            "Cannot verify BCD store without bcdedit or hivexregedit (run `aero-win7-slipstream deps`)"
        )
    })?;
    let exported = run_capture(Command::new(hivex).arg("--export").arg(store), verbose)
        .context("hivexregedit export failed")?;
    if !bcd::hive_contains_policy(&exported, mode) {
        return Err(anyhow!(
            "BCD hive export did not contain expected policy flag for mode {:?}",
            mode
        ));
    }
    Ok(())
}

fn run_capture(cmd: &mut Command, verbose: bool) -> Result<String> {
    if verbose {
        eprintln!("> {:?}", cmd);
    }
    let out = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .context("Failed to spawn external command")?;
    if !out.status.success() {
        return Err(anyhow!("External command failed with status: {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}
