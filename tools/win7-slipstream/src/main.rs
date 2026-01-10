mod bcd;
mod cli;
mod deps;
mod hash;
mod iso;
mod manifest;
mod unattend;
mod verify;
mod wim;

use crate::cli::{BackendKind, Cli, Command, PatchIsoArgs};
use crate::deps::DepContext;
use crate::iso::{IsoBuilder, IsoExtractor};
use crate::manifest::{Manifest, PatchedPath};
use crate::unattend::{render_autounattend, UnattendMode};
use crate::wim::{Backend, CertInfo, SigningMode};
use anyhow::{anyhow, Context, Result};
use clap::Parser;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let ctx = DepContext::detect();

    match cli.command {
        Command::Deps => {
            deps::print_deps(&ctx);
            Ok(())
        }
        Command::PatchIso(args) => patch_iso(&ctx, &args),
        Command::VerifyIso(args) => verify::run_verify(&ctx, args),
    }
}

fn patch_iso(ctx: &DepContext, args: &PatchIsoArgs) -> Result<()> {
    let input_iso = args
        .input
        .canonicalize()
        .with_context(|| format!("Failed to resolve input ISO path: {}", args.input.display()))?;
    let output_iso = args.output.clone();
    let drivers_root = args
        .drivers
        .canonicalize()
        .with_context(|| format!("Failed to resolve drivers path: {}", args.drivers.display()))?;

    if !input_iso.is_file() {
        return Err(anyhow!("Input ISO is not a file: {}", input_iso.display()));
    }
    if !drivers_root.is_dir() {
        return Err(anyhow!(
            "Driver pack root is not a directory: {}",
            drivers_root.display()
        ));
    }

    let mut workdir_temp: Option<TempDir> = None;
    let workdir: PathBuf = if let Some(dir) = &args.workdir {
        dir.clone()
    } else {
        let tmp = TempDir::new().context("Failed to create temporary workdir")?;
        let path = tmp.path().to_path_buf();
        workdir_temp = Some(tmp);
        path
    };

    if args.verbose {
        eprintln!("workdir: {}", workdir.display());
    }

    let iso_root = workdir.join("iso-root");
    if iso_root.exists() {
        fs::remove_dir_all(&iso_root).with_context(|| {
            format!(
                "Failed to clear existing workdir ISO root: {}",
                iso_root.display()
            )
        })?;
    }
    fs::create_dir_all(&iso_root).with_context(|| {
        format!(
            "Failed to create workdir ISO root directory: {}",
            iso_root.display()
        )
    })?;

    let extractor = IsoExtractor::detect(ctx).context("No supported ISO extractor found")?;
    extractor
        .extract(&input_iso, &iso_root, args.verbose)
        .context("Failed to extract input ISO")?;
    iso::make_tree_writable(&iso_root).context("Failed to make extracted ISO tree writable")?;

    let arch = wim::resolve_arch(&iso_root, &args.arch).context("Failed to resolve arch")?;
    let signing_mode = args.signing_mode;

    let cert_info = match signing_mode {
        SigningMode::TestSigning => {
            let cert_path = args.cert.as_ref().ok_or_else(|| {
                anyhow!(
                    "--cert is required when --signing-mode testsigning (no in-repo cert is shipped)"
                )
            })?;
            Some(CertInfo::from_path(cert_path).context("Failed to read certificate")?)
        }
        _ => None,
    };

    let backend_kind = BackendKind::resolve(ctx, args.backend).context("Failed to select backend")?;
    let backend_workdir = workdir.join("backend");
    let backend = Backend::new_with_workdir(backend_kind, ctx, &backend_workdir, args.verbose)?;

    let mut patched_paths: Vec<PatchedPath> = Vec::new();

    let driver_src = wim::select_driver_dir(&drivers_root, arch).context("Failed to locate driver pack arch directory")?;
    let driver_dst_rel = Path::new("AERO").join("DRIVERS").join(arch.iso_dir_name());
    let driver_dst = iso_root.join(&driver_dst_rel);
    wim::copy_dir_recursive(&driver_src, &driver_dst, &driver_dst_rel, &mut patched_paths)
        .context("Failed to copy drivers into ISO tree")?;

    patched_paths.push(PatchedPath::new_dir(
        driver_dst_rel.to_string_lossy().to_string(),
    ));

    let unattend_mode = args.unattend;
    if unattend_mode != UnattendMode::None {
        let unattend_xml = render_autounattend(arch, driver_dst_rel.to_string_lossy().as_ref(), unattend_mode)?;
        let unattend_path = iso_root.join("autounattend.xml");
        fs::write(&unattend_path, unattend_xml).with_context(|| {
            format!(
                "Failed to write autounattend.xml to {}",
                unattend_path.display()
            )
        })?;
        patched_paths.push(PatchedPath::new_file("autounattend.xml"));
    }

    let bcd_paths = [
        iso_root.join("boot").join("BCD"),
        iso_root
            .join("efi")
            .join("microsoft")
            .join("boot")
            .join("BCD"),
    ];
    for bcd_path in &bcd_paths {
        if bcd_path.is_file() {
            backend
                .patch_bcd_store(bcd_path, signing_mode)
                .with_context(|| format!("Failed to patch BCD store: {}", bcd_path.display()))?;
            if let Ok(rel) = bcd_path.strip_prefix(&iso_root) {
                patched_paths.push(PatchedPath::new_file(rel.to_string_lossy().to_string()));
            }
        }
    }

    let sources_dir = iso_root.join("sources");
    let boot_wim = sources_dir.join("boot.wim");
    let install_wim = sources_dir.join("install.wim");
    if boot_wim.is_file() {
        if let Some(cert) = cert_info.as_ref() {
            backend
                .inject_cert_into_wim(&boot_wim, &[2], cert)
                .context("Failed to inject certificate into boot.wim")?;
            patched_paths.push(PatchedPath::new_file("sources/boot.wim"));
        }

        if backend.supports_offline_driver_injection() {
            backend
                .inject_drivers_into_wim(&boot_wim, &[2], &driver_src, signing_mode)
                .context("Failed to inject drivers into boot.wim")?;
            patched_paths.push(PatchedPath::new_file("sources/boot.wim"));
        }
    }

    if install_wim.is_file() {
        let install_indexes = backend
            .wim_indexes(&install_wim)
            .context("Failed to enumerate install.wim indexes")?;

        backend
            .patch_install_wim_bcd_template(&install_wim, &install_indexes, signing_mode)
            .context("Failed to patch install.wim BCD-Template")?;
        patched_paths.push(PatchedPath::new_file("sources/install.wim"));

        if let Some(cert) = cert_info.as_ref() {
            backend
                .inject_cert_into_wim(&install_wim, &install_indexes, cert)
                .context("Failed to inject certificate into install.wim")?;
            patched_paths.push(PatchedPath::new_file("sources/install.wim"));
        }

        if backend.supports_offline_driver_injection() {
            backend
                .inject_drivers_into_wim(&install_wim, &install_indexes, &driver_src, signing_mode)
                .context("Failed to inject drivers into install.wim")?;
            patched_paths.push(PatchedPath::new_file("sources/install.wim"));
        }
    }

    let input_iso_sha256 = hash::sha256_file(&input_iso).context("Failed to hash input ISO")?;
    let driver_pack_sha256 = hash::sha256_dir(&drivers_root).context("Failed to hash driver pack")?;

    let cert_manifest = cert_info.as_ref().map(|cert| cert.as_manifest());

    patched_paths.push(PatchedPath::new_file("AERO/MANIFEST.json"));
    let manifest = Manifest {
        tool: manifest::ToolInfo::current(),
        input_iso_sha256,
        driver_pack_sha256,
        signing_mode,
        arch,
        backend: backend_kind,
        unattend: unattend_mode,
        certificate: cert_manifest,
        patched_paths,
    };

    let manifest_path_rel = Path::new("AERO").join("MANIFEST.json");
    let manifest_path = iso_root.join(&manifest_path_rel);
    fs::create_dir_all(manifest_path.parent().unwrap()).context("Failed to create AERO directory")?;
    fs::write(&manifest_path, manifest.to_json_pretty()?).with_context(|| {
        format!(
            "Failed to write manifest file to {}",
            manifest_path.display()
        )
    })?;

    let builder = IsoBuilder::detect(ctx).context("No supported ISO builder found")?;
    builder
        .build(&input_iso, &iso_root, &output_iso, args.verbose)
        .context("Failed to rebuild ISO")?;

    if args.keep_workdir {
        if let Some(tmp) = workdir_temp.take() {
            eprintln!("Keeping workdir: {}", workdir.display());
            let _ = tmp.keep();
        } else {
            eprintln!("Using workdir: {}", workdir.display());
        }
    }

    Ok(())
}
