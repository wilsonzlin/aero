# Windows 7 Offline Media Patcher (testsigning + certificate injection)

This repository includes a Windows-only helper that patches a **user-provided, extracted Windows 7 SP1 ISO** directory so it can boot and install using **test-signed drivers** (most importantly: boot-critical storage drivers).

Recommended script:

- `tools/windows/patch-win7-media.ps1` (see [`tools/windows/README.md`](../tools/windows/README.md))

The patcher **does not ship any Microsoft binaries/images**; it only edits the extracted ISO files you point it at.

It can:

- Enable `testsigning` (and optionally `nointegritychecks`) in the install-media BCD store(s)
- Patch `BCD-Template` inside `install.wim` so the installed OS inherits the same boot policy
- Inject a public signing certificate into offline `ROOT` + `TrustedPublisher` (by default) inside:
  - `boot.wim` (WinPE/Setup)
  - `install.wim` (installed OS)
- Optionally inject drivers from a directory containing `.inf` files (via `-DriversPath`)

See also:

- [`docs/16-win7-image-servicing.md`](./16-win7-image-servicing.md) (background + manual workflow)
- [`docs/16-windows7-install-media-prep.md`](./16-windows7-install-media-prep.md) (auditable, longer-form guide)

---

## Prerequisites

- Windows 10/11 host (recommended)
- Run from an **elevated** PowerShell prompt (Administrator)
- PowerShell 5.1+ (Windows PowerShell or PowerShell 7)
- Built-in tools:
  - `dism.exe`
  - `bcdedit.exe`
  - `attrib.exe`
  - `reg.exe`
- `win-offline-cert-injector.exe` (build once from `tools/win-offline-cert-injector/`)

```powershell
cd tools\win-offline-cert-injector
cargo build --release --locked
```

---

## Usage

1) Extract a Windows 7 SP1 ISO to a folder (example: `C:\win7-iso\`).

2) Run the patch script as Administrator.

Example (CI-style test-signed drivers + cert):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\win7-iso `
  -CertPath  .\out\certs\aero-test.cer `
  -DriversPath .\out\packages
```

Example (patch signing policy + cert trust only; no driver injection):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\win7-iso `
  -CertPath  C:\path\to\driver-test.cer
```

---

## Optional flags

- Enable `nointegritychecks` as well (**not recommended**; only for lab bring-up):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\win7-iso `
  -CertPath  C:\path\to\driver-test.cer `
  -EnableNoIntegrityChecks
```

- Patch only `boot.wim` Setup image (index 2):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\win7-iso `
  -CertPath  C:\path\to\driver-test.cer `
  -BootWimIndices 2
```

- Patch a subset of `install.wim` indices:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\win7-iso `
  -CertPath  C:\path\to\driver-test.cer `
  -InstallWimIndices "1,4"
```

- Inject the certificate into additional stores (example: `TrustedPeople` + `CA`):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\win7-iso `
  -CertPath  C:\path\to\driver-test.cer `
  -CertStores ROOT,CA,TrustedPublisher,TrustedPeople
```

---

## What gets patched (high level)

### BCD stores on the extracted ISO folder

Patched via `bcdedit /store`:

- `boot\BCD` (BIOS/CSM)
- `efi\microsoft\boot\bcd` (UEFI, if present)

The script enables:

- `testsigning on`
- `nointegritychecks on` only when `-EnableNoIntegrityChecks` is passed

### Offline registry hives inside WIM images (certificate trust)

For each selected mounted WIM index, the script calls `win-offline-cert-injector` to inject the certificate into the offline `SOFTWARE` hive under:

- `Microsoft\SystemCertificates\ROOT`
- `Microsoft\SystemCertificates\TrustedPublisher`

### `BCD-Template` inside `install.wim` (installed OS boot policy)

Patched via `bcdedit /store`:

- `<MountDir>\Windows\System32\Config\BCD-Template`

---

## Verification

From the patched ISO folder on the host:

```powershell
bcdedit /store C:\win7-iso\boot\BCD /enum {default}
if (Test-Path C:\win7-iso\efi\microsoft\boot\bcd) {
  bcdedit /store C:\win7-iso\efi\microsoft\boot\bcd /enum {default}
}
```

In WinPE/Setup or the installed OS:

```cmd
bcdedit /enum {current}
certutil -store Root
certutil -store TrustedPublisher
```

---

## Notes / Safety

- The script **modifies files in place**. Work on a copy of your extracted ISO directory if you want to preserve the original.
- If a run is interrupted, you may need to clean up stuck WIM mounts manually (`dism /Cleanup-Wim`).
