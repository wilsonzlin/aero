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

## Sign driver packages (catalogs)

Signing typically targets the `.cat` file produced by `Inf2Cat`. The Windows Driver Kit (WDK) provides `signtool.exe`.

Example:

```powershell
signtool sign /fd sha256 /a /f .\dist\certs\aero-virtio-test.pfx .\path\to\driver.cat
```

Automation for signing is intentionally not fully baked here because it depends on your WDK install path and build pipeline.

