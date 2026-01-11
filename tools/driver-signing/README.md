# Windows driver test-signing tooling (Win7 x64)

These scripts are intended for **developer/test builds** when using unsigned or self-built drivers.

If you use a virtio-win distribution that already includes signed catalogs (`.cat`), Windows 7 x64 should install them without enabling test mode.

## Enable test-signing mode (Win7 x64)

Run in an elevated Command Prompt:

```bat
bcdedit /set testsigning on
shutdown /r /t 0
```

Disable:

```bat
bcdedit /set testsigning off
shutdown /r /t 0
```

## Create a test code-signing certificate

On a modern Windows host with PowerShell and `New-SelfSignedCertificate` available:

```powershell
.\tools\driver-signing\new-test-cert.ps1 -OutDir .\dist\certs
```

This produces:

- `aero-virtio-test.cer` (public cert)
- `aero-virtio-test.pfx` (private key bundle, password prompted)

Import the certificate into the target VM (Trusted Root + Trusted Publishers) before installing drivers.

### SHA-1 vs SHA-2 (Windows 7 compatibility)

Windows 7 SP1 without SHA-2 updates (notably **KB3033929** / **KB4474419**) can fail to validate driver signatures even if you sign files with `/fd SHA1` *if the certificate itself is SHA-256-signed*.

For maximum out-of-box Windows 7 compatibility:

- Create the cert with a **SHA-1 signature algorithm** (this script defaults to `-CertHashAlgorithm sha1`).
- Sign files with `/fd SHA1`, or dual-sign (SHA-1 first, then append SHA-256).

If your signing machine refuses SHA-1 certificate creation, `new-test-cert.ps1` will fail unless you explicitly opt into the compatibility risk via `-AllowSha2CertFallback` (generates a SHA-256-signed certificate and prints warnings).

### Offline injection (WinPE / setup / first boot)

If you need the signing chain trusted **before** driver load (e.g. WinPE / Windows Setup / first boot),
inject the certificate into an **offline** Windows image by editing the offline `SOFTWARE` hive:

```powershell
cd tools\win-offline-cert-injector
cargo build --release --locked

.\target\release\win-offline-cert-injector.exe `
  --windows-dir X:\mount `
  .\dist\certs\aero-virtio-test.cer
```

By default this injects into `ROOT` + `TrustedPublisher`. Use `--store` to override.

## Sign driver packages (catalogs)

Signing typically targets the `.cat` file produced by `Inf2Cat`. The Windows Driver Kit (WDK) provides `signtool.exe`.

Example:

```powershell
signtool sign /fd sha1 /a /f .\dist\certs\aero-virtio-test.pfx .\path\to\driver.cat
```

Automation for signing is intentionally not fully baked here because it depends on your WDK install path and build pipeline.
