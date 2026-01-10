use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::Engine;
use clap::{Parser, ValueEnum};
use serde::Serialize;

mod export;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum StoreName {
    Root,
    TrustedPublisher,
    TrustedPeople,
}

impl StoreName {
    fn as_str(self) -> &'static str {
        match self {
            StoreName::Root => "ROOT",
            StoreName::TrustedPublisher => "TrustedPublisher",
            StoreName::TrustedPeople => "TrustedPeople",
        }
    }
}

impl std::str::FromStr for StoreName {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let normalized = s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_'], "");
        match normalized.as_str() {
            "root" => Ok(StoreName::Root),
            "trustedpublisher" => Ok(StoreName::TrustedPublisher),
            "trustedpeople" => Ok(StoreName::TrustedPeople),
            _ => Err(format!(
                "invalid store name {s:?} (expected ROOT, TrustedPublisher, or TrustedPeople)"
            )),
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Json,
    Reg,
}

#[derive(Parser, Debug)]
#[command(about = "Export CryptoAPI-generated SystemCertificates registry blobs for a certificate")]
struct Args {
    /// SystemCertificates store name (repeatable). Supported: ROOT, TrustedPublisher, TrustedPeople.
    #[arg(short, long, required = true)]
    store: Vec<StoreName>,

    /// Output format (json writes to stdout; reg writes a .reg snippet to stdout)
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    format: OutputFormat,

    /// Additionally write a .reg snippet to this file (only valid when --format json)
    #[arg(long)]
    reg_out: Option<PathBuf>,

    /// Certificate file(s) in PEM or DER
    #[arg(value_name = "CERT_FILE", required = true)]
    cert_files: Vec<PathBuf>,
}

#[derive(Serialize)]
struct PatchJson {
    store: String,
    thumbprint_sha1: String,
    values: BTreeMap<String, String>,
}

fn patch_to_json(patch: &export::CertRegistryPatch) -> PatchJson {
    PatchJson {
        store: patch.store.clone(),
        thumbprint_sha1: patch.thumbprint_sha1.clone(),
        values: patch
            .values
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    base64::engine::general_purpose::STANDARD.encode(&v.bytes),
                )
            })
            .collect(),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.format == OutputFormat::Reg && args.reg_out.is_some() {
        anyhow::bail!("--reg-out is only valid with --format json");
    }

    let mut patches = Vec::new();
    for cert_file in &args.cert_files {
        let ders =
            export::load_certificates_from_file(cert_file).with_context(|| format!("{cert_file:?}"))?;
        for der in ders {
            for store in &args.store {
                patches.push(
                    export::export_system_cert_reg_patch(store.as_str(), &der)
                        .with_context(|| format!("store {}", store.as_str()))?,
                );
            }
        }
    }

    match args.format {
        OutputFormat::Json => {
            let json_value = if patches.len() == 1 {
                serde_json::to_value(patch_to_json(&patches[0]))?
            } else {
                let list: Vec<_> = patches.iter().map(patch_to_json).collect();
                serde_json::to_value(list)?
            };

            println!("{}", serde_json::to_string_pretty(&json_value)?);

            if let Some(path) = &args.reg_out {
                let reg = export::render_reg_file(&patches)?;
                std::fs::write(path, reg).with_context(|| format!("write {}", path.display()))?;
            }
        }
        OutputFormat::Reg => {
            print!("{}", export::render_reg_file(&patches)?);
        }
    }

    Ok(())
}
