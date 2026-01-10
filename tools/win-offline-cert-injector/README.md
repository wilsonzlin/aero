# win-offline-cert-injector

Windows-native CLI tool to inject one or more X.509 certificates into an **offline** Windows `LocalMachine` certificate store (SystemCertificates registry store) by editing an offline `SOFTWARE` registry hive in-place.

This is primarily intended for WinPE / setup / first-boot scenarios where test-signed drivers must be trusted *before* driver load.

## Build

```powershell
cd tools\win-offline-cert-injector
cargo build --release
```

## Usage

```text
win-offline-cert-injector --hive <path-to-SOFTWARE> [--store <STORE> ...] [--verify-only] <cert-file>...
win-offline-cert-injector --windows-dir <mount-root> [--store <STORE> ...] [--verify-only] <cert-file>...

Stores (case-insensitive):
  ROOT
  TrustedPublisher
  TrustedPeople
```

Notes:
- Default stores (when `--store` is not provided): `ROOT` + `TrustedPublisher`.
- Certificate inputs may be DER (`.cer`) or PEM. PEM files may contain multiple `BEGIN CERTIFICATE` blocks; **all** are processed.
- The tool must be run elevated (Administrator) so it can enable `SeRestorePrivilege` + `SeBackupPrivilege` for `RegLoadKeyW`/`RegUnLoadKeyW`.

### Examples

Inject into the default stores (`ROOT` + `TrustedPublisher`) using an explicit hive path:

```powershell
win-offline-cert-injector `
  --hive X:\mount\Windows\System32\config\SOFTWARE `
  .\aero-root.cer .\aero-publisher.cer
```

Inject into `TrustedPeople` only:

```powershell
win-offline-cert-injector `
  --windows-dir X:\mount `
  --store TrustedPeople `
  .\aero-signer.cer
```

Verify-only (does not modify the hive):

```powershell
win-offline-cert-injector `
  --hive X:\mount\Windows\System32\config\SOFTWARE `
  --verify-only `
  .\aero-root.cer
```

## Manual verification

1. Mount a WinPE/Win7 image to (for example) `X:\mount`.
2. Run `win-offline-cert-injector` to inject your certificates.
3. Re-load the hive and confirm the registry subkey exists:

```powershell
reg load HKLM\AERO_OFFLINE_SOFTWARE X:\mount\Windows\System32\config\SOFTWARE

reg query `
  HKLM\AERO_OFFLINE_SOFTWARE\Microsoft\SystemCertificates\ROOT\Certificates\<THUMBPRINT> `
  /v Blob

reg unload HKLM\AERO_OFFLINE_SOFTWARE
```

The tool prints each certificate SHA1 thumbprint it injected; that thumbprint is the registry subkey name.

