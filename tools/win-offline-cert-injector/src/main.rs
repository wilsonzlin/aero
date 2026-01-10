#[cfg(any(windows, test))]
mod pem;

#[cfg(windows)]
mod winapi;

#[cfg(windows)]
use std::path::{Path, PathBuf};

#[cfg(windows)]
#[derive(Debug)]
enum ToolError {
    Usage(String),
    Io(std::io::Error),
    Pem(String),
    #[cfg(windows)]
    Win(winapi::WinError),
}

#[cfg(windows)]
impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(msg) => write!(f, "{msg}"),
            Self::Io(err) => write!(f, "{err}"),
            Self::Pem(err) => write!(f, "{err}"),
            #[cfg(windows)]
            Self::Win(err) => write!(f, "{err}"),
        }
    }
}

#[cfg(windows)]
impl std::error::Error for ToolError {}

#[cfg(windows)]
impl From<std::io::Error> for ToolError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

#[cfg(windows)]
impl From<winapi::WinError> for ToolError {
    fn from(err: winapi::WinError) -> Self {
        Self::Win(err)
    }
}

#[cfg(windows)]
struct Cli {
    hive_path: PathBuf,
    stores: Vec<String>,
    verify_only: bool,
    cert_files: Vec<PathBuf>,
}

#[cfg(windows)]
const DEFAULT_STORES: &[&str] = &["ROOT", "TrustedPublisher"];

#[cfg(windows)]
fn usage() -> &'static str {
    "win-offline-cert-injector\n\
\n\
Usage:\n\
  win-offline-cert-injector --hive <path-to-SOFTWARE> [--store <STORE> ...] [--verify-only] [--cert <cert-file> ...] [<cert-file>...]\n\
  win-offline-cert-injector --windows-dir <mount-root> [--store <STORE> ...] [--verify-only] [--cert <cert-file> ...] [<cert-file>...]\n\
\n\
Stores (case-insensitive): ROOT, TrustedPublisher, TrustedPeople\n\
Default stores: ROOT + TrustedPublisher\n"
}

#[cfg(windows)]
fn normalize_store_name(input: &str) -> Result<&'static str, ToolError> {
    let upper = input.to_ascii_uppercase();
    match upper.as_str() {
        "ROOT" => Ok("ROOT"),
        "TRUSTEDPUBLISHER" => Ok("TrustedPublisher"),
        "TRUSTEDPEOPLE" => Ok("TrustedPeople"),
        _ => Err(ToolError::Usage(format!(
            "unknown store: {input}\n\n{}",
            usage()
        ))),
    }
}

#[cfg(windows)]
fn hex_upper(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

#[cfg(windows)]
fn parse_args() -> Result<Option<Cli>, ToolError> {
    let mut hive: Option<PathBuf> = None;
    let mut windows_dir: Option<PathBuf> = None;
    let mut stores: Vec<String> = Vec::new();
    let mut verify_only = false;
    let mut cert_files: Vec<PathBuf> = Vec::new();

    let mut args = std::env::args_os().skip(1).peekable();
    while let Some(arg) = args.next() {
        let Some(arg_str) = arg.to_str() else {
            return Err(ToolError::Usage(format!(
                "invalid argument (non-utf8)\n\n{}",
                usage()
            )));
        };

        match arg_str {
            "-h" | "--help" => {
                print!("{}", usage());
                return Ok(None);
            }
            "--hive" => {
                let val = args.next().ok_or_else(|| {
                    ToolError::Usage(format!("--hive requires a value\n\n{}", usage()))
                })?;
                hive = Some(PathBuf::from(val));
            }
            "--windows-dir" => {
                let val = args.next().ok_or_else(|| {
                    ToolError::Usage(format!("--windows-dir requires a value\n\n{}", usage()))
                })?;
                windows_dir = Some(PathBuf::from(val));
            }
            "--store" => {
                let val = args.next().ok_or_else(|| {
                    ToolError::Usage(format!("--store requires a value\n\n{}", usage()))
                })?;
                let Some(val_str) = val.to_str() else {
                    return Err(ToolError::Usage(format!(
                        "--store value must be utf-8\n\n{}",
                        usage()
                    )));
                };
                let store = normalize_store_name(val_str)?.to_string();
                if !stores.iter().any(|s| s == &store) {
                    stores.push(store);
                }
            }
            "--cert" => {
                let val = args.next().ok_or_else(|| {
                    ToolError::Usage(format!("--cert requires a value\n\n{}", usage()))
                })?;
                cert_files.push(PathBuf::from(val));
            }
            "--verify-only" => verify_only = true,
            _ if arg_str.starts_with("--") => {
                return Err(ToolError::Usage(format!(
                    "unknown flag: {arg_str}\n\n{}",
                    usage()
                )));
            }
            _ => cert_files.push(PathBuf::from(arg)),
        }
    }

    if hive.is_some() && windows_dir.is_some() {
        return Err(ToolError::Usage(format!(
            "provide only one of --hive or --windows-dir\n\n{}",
            usage()
        )));
    }

    let hive_path = match (hive, windows_dir) {
        (Some(hive), None) => hive,
        (None, Some(dir)) => resolve_hive_from_windows_dir(&dir)?,
        (None, None) => {
            return Err(ToolError::Usage(format!(
                "missing required flag: --hive or --windows-dir\n\n{}",
                usage()
            )))
        }
        (Some(_), Some(_)) => unreachable!(),
    };

    if cert_files.is_empty() {
        return Err(ToolError::Usage(format!(
            "expected at least one certificate file\n\n{}",
            usage()
        )));
    }

    if stores.is_empty() {
        stores = DEFAULT_STORES.iter().map(|s| s.to_string()).collect();
    }

    Ok(Some(Cli {
        hive_path,
        stores,
        verify_only,
        cert_files,
    }))
}

#[cfg(windows)]
fn resolve_hive_from_windows_dir(dir: &Path) -> Result<PathBuf, ToolError> {
    let candidate = dir
        .join("Windows")
        .join("System32")
        .join("config")
        .join("SOFTWARE");
    if candidate.exists() {
        return Ok(candidate);
    }
    let candidate = dir.join("System32").join("config").join("SOFTWARE");
    if candidate.exists() {
        return Ok(candidate);
    }

    Err(ToolError::Usage(format!(
        "could not find offline SOFTWARE hive under: {}\n\n{}",
        dir.display(),
        usage()
    )))
}

#[cfg(not(windows))]
fn main() {
    eprintln!("win-offline-cert-injector only runs on Windows");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
fn run() -> Result<std::process::ExitCode, ToolError> {
    let Some(cli) = parse_args()? else {
        return Ok(std::process::ExitCode::SUCCESS);
    };

    winapi::enable_required_privileges()?;
    let mount_name = winapi::choose_unique_mount_name("AERO_OFFLINE_SOFTWARE")?;
    let mut hive = winapi::LoadedHive::load(&cli.hive_path, &mount_name)?;

    let exit_code = {
        let hive_root =
            winapi::RegKey::open(winapi::HKEY_LOCAL_MACHINE, &mount_name, !cli.verify_only)?;
        let inputs = load_all_input_certs(&cli.cert_files)?;

        if cli.verify_only {
            verify_all(&hive_root, &cli.stores, &inputs)?
        } else {
            inject_all(&hive_root, &cli.stores, &inputs)?
        }
    };

    hive.unload()?;
    Ok(exit_code)
}

#[cfg(windows)]
struct InputCert {
    source_path: PathBuf,
    source_index: usize,
    der: Vec<u8>,
    thumbprint_hex: String,
}

#[cfg(windows)]
fn load_all_input_certs(cert_files: &[PathBuf]) -> Result<Vec<InputCert>, ToolError> {
    let mut out = Vec::new();
    for path in cert_files {
        let data = std::fs::read(path)?;
        let ders = pem::decode_cert_file(&data).map_err(ToolError::Pem)?;
        for (idx, der) in ders.into_iter().enumerate() {
            let sha1 = winapi::cert_sha1_thumbprint(&der)?;
            out.push(InputCert {
                source_path: path.clone(),
                source_index: idx,
                der,
                thumbprint_hex: hex_upper(&sha1),
            });
        }
    }
    Ok(out)
}

#[cfg(windows)]
fn inject_all(
    hive_root: &winapi::RegKey,
    stores: &[String],
    certs: &[InputCert],
) -> Result<std::process::ExitCode, ToolError> {
    for store in stores {
        let store_key = winapi::RegKey::create_path(
            hive_root.raw(),
            &format!("Microsoft\\SystemCertificates\\{store}"),
        )?;
        let _ = winapi::RegKey::create(store_key.raw(), "Certificates")?;
        let cert_store = winapi::CertStore::open_system_registry(store_key.raw(), false)?;
        for cert in certs {
            winapi::cert_add_encoded_cert(&cert_store, &cert.der)?;
            println!(
                "{store}\t{}\t{}{}",
                &cert.thumbprint_hex,
                cert.source_path.display(),
                if cert.source_index == 0 {
                    "".to_string()
                } else {
                    format!("#{}", cert.source_index + 1)
                }
            );
        }
    }

    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(windows)]
fn verify_all(
    hive_root: &winapi::RegKey,
    stores: &[String],
    certs: &[InputCert],
) -> Result<std::process::ExitCode, ToolError> {
    let mut missing = false;
    for store in stores {
        for cert in certs {
            let subkey = format!(
                "Microsoft\\SystemCertificates\\{store}\\Certificates\\{}",
                cert.thumbprint_hex
            );
            let exists = match winapi::RegKey::open(hive_root.raw(), &subkey, false) {
                Ok(_) => true,
                Err(err) if err.code == 2 || err.code == 3 => false,
                Err(err) => return Err(ToolError::Win(err)),
            };
            if !exists {
                missing = true;
            }
            println!(
                "{store}\t{}\t{}\t{}{}",
                if exists { "FOUND" } else { "MISSING" },
                &cert.thumbprint_hex,
                cert.source_path.display(),
                if cert.source_index == 0 {
                    "".to_string()
                } else {
                    format!("#{}", cert.source_index + 1)
                }
            );
        }
    }

    Ok(if missing {
        std::process::ExitCode::from(2)
    } else {
        std::process::ExitCode::SUCCESS
    })
}

#[cfg(all(windows, test))]
mod windows_smoke_tests {
    use super::*;

    const TEST_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIDUzCCAjugAwIBAgIUICH8ziCYcB5uSzL9TOKgv2ixnGgwDQYJKoZIhvcNAQEL\n\
BQAwOTEoMCYGA1UEAwwfQWVybyBPZmZsaW5lIENlcnQgSW5qZWN0b3IgVGVzdDEN\n\
MAsGA1UECgwEQWVybzAeFw0yNjAxMTAxMTM1NTlaFw0zNjAxMDgxMTM1NTlaMDkx\n\
KDAmBgNVBAMMH0Flcm8gT2ZmbGluZSBDZXJ0IEluamVjdG9yIFRlc3QxDTALBgNV\n\
BAoMBEFlcm8wggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQC36peoVuu4\n\
l9MeGecg1Et96wPum/ic4O7Zx4FIaJTeH2YcvWK3/u9yRfCugCmEEWW/RxCLIhqB\n\
GqBRvgJcipYDOGnBjhwD/9CsADrjvMLZsxqttTAeSIhkyjq9i4xD/D9p2mSIw5gj\n\
+MSxflQ9tNlnRzLlDlYLrvwP+vvrd1MwCw5fJF6BP6Jb/FapQjDZbuVYgxvy1aiS\n\
KrXK576Z+LOl5UFoi2bljdtVBfa51+s/wt5E6D6VwNMoOp5mH2Nyz4wazibpxECx\n\
HYEu5+9cWcjuNsjVhckNExs/p8r19WExGvnLMZaW3kla7VqGijSJ9912rTeOjp8A\n\
X/yxDnRcu/OlAgMBAAGjUzBRMB0GA1UdDgQWBBSFKMOlLLiu5TNqWQOYdNGVRFB/\n\
VjAfBgNVHSMEGDAWgBSFKMOlLLiu5TNqWQOYdNGVRFB/VjAPBgNVHRMBAf8EBTAD\n\
AQH/MA0GCSqGSIb3DQEBCwUAA4IBAQAGiOM4IRGShmI3cp77/Fcbld3HY1a2xYAj\n\
TWSzgDkZAyRvfIWwVs2oc6o/BPeVe0lHRUuRsI8L+Flqe+OnUNz7ePneOi7V1YJ3\n\
Jj6Ahb3yiPF/g4hGXkq8oXEvoTbXw3ah8KeK+PEbG6e0VC7oiCkxqI8JkOsJaOGW\n\
S48MU/8ElN1JL71zP6LR69thzOqdP4ihrjs7R+BKBblkiSCZcx6kG/65HHUlLO9c\n\
bYJVna6IxwbidSCB6WtEieXNX88nMksxvnqZZH7dXf3Cjb/SoHHTWJa5C2BWsAm+\n\
3HEgrHfJAffW5IeZWaFcGTBpWwDGMaoocVBdkLngqv1qw097zWWo\n\
-----END CERTIFICATE-----\n";

    fn temp_hive_path() -> PathBuf {
        let pid = std::process::id();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("aero_offline_cert_injector_test_{pid}_{now}.hiv"))
    }

    #[test]
    fn offline_hive_inject_and_verify() {
        if let Err(err) = winapi::enable_required_privileges() {
            eprintln!("skipping offline hive smoke test (privileges unavailable): {err}");
            return;
        }

        let hive_path = temp_hive_path();
        let _ = std::fs::remove_file(&hive_path);

        // Create a minimal hive file by saving a temporary HKCU key. The resulting file can be
        // loaded via RegLoadKeyW and is sufficient for exercising offline injection.
        let pid = std::process::id();
        let test_key_path = format!("Software\\AeroOfflineCertInjectorTest\\{pid}");
        let test_leaf_path = format!(
            "{test_key_path}\\{}",
            hive_path.file_stem().unwrap().to_string_lossy()
        );
        let test_key =
            winapi::RegKey::create_path(winapi::HKEY_CURRENT_USER, &test_leaf_path).unwrap();
        winapi::reg_save_key(test_key.raw(), &hive_path).unwrap();
        drop(test_key);
        let _ = winapi::reg_delete_tree(winapi::HKEY_CURRENT_USER, &test_key_path);

        let der = pem::decode_cert_file(TEST_CERT_PEM.as_bytes())
            .expect("parse test cert PEM")
            .into_iter()
            .next()
            .expect("test cert PEM contained one certificate");
        let sha1 = winapi::cert_sha1_thumbprint(&der).expect("compute test cert SHA1 thumbprint");
        let certs = vec![InputCert {
            source_path: PathBuf::from("<embedded>"),
            source_index: 0,
            der,
            thumbprint_hex: hex_upper(&sha1),
        }];

        let stores = vec![
            "ROOT".to_string(),
            "TrustedPublisher".to_string(),
            "TrustedPeople".to_string(),
        ];

        let mount_name = winapi::choose_unique_mount_name("AERO_OFFLINE_SOFTWARE_TEST")
            .expect("choose unique hive mount name");
        let mut hive = winapi::LoadedHive::load(&hive_path, &mount_name).expect("load hive");
        {
            let hive_root =
                winapi::RegKey::open(winapi::HKEY_LOCAL_MACHINE, &mount_name, true).unwrap();

            let before = verify_all(&hive_root, &stores, &certs).unwrap();
            assert_eq!(before.code(), Some(2));

            let inject1 = inject_all(&hive_root, &stores, &certs).unwrap();
            assert_eq!(inject1.code(), Some(0));

            let after = verify_all(&hive_root, &stores, &certs).unwrap();
            assert_eq!(after.code(), Some(0));

            // Idempotency: adding again should not fail.
            let inject2 = inject_all(&hive_root, &stores, &certs).unwrap();
            assert_eq!(inject2.code(), Some(0));
        }
        hive.unload().expect("unload hive");

        let _ = std::fs::remove_file(&hive_path);
    }
}
