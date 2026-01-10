# Windows 7 Offline Media Patcher (testsigning + Aero test certificate)

This repository includes a Windows-first patch script that modifies a **user-provided, extracted Windows 7 SP1 ISO** folder so that:

- WinPE/Setup boots with `testsigning` enabled (and `nointegritychecks` enabled by default).
- `boot.wim` (WinPE) and `install.wim` (installed OS) trust a provided test certificate in:
  - `LocalMachine\Root` (Trusted Root Certification Authorities)
  - `LocalMachine\TrustedPublisher`
- The installed OS defaults to `testsigning` (and optionally `nointegritychecks`) by patching `BCD-Template` inside `install.wim`.

The patcher **does not ship any Microsoft binaries/images**; it only edits the files you point it at.

---

## Prerequisites

- Windows host (recommended: Windows 10/11)
- Administrator shell
- Windows PowerShell 5.1+
- Built-in tools available in `PATH`:
  - `dism.exe` (WIM mount/unmount)
  - `bcdedit.exe` (BCD store editing)
  - `reg.exe` (offline hive load/unload)

---

## Usage

1) Extract a Windows 7 SP1 ISO to a folder (example: `C:\win7-iso\`).

2) Run the patch script as Administrator:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\scripts\patch-win7-media.ps1 `
  -IsoRoot 'C:\win7-iso' `
  -CertPath 'C:\path\to\aero-test.cer'
```

### Optional flags

- Disable `nointegritychecks` (default is enabled):

```powershell
.\scripts\patch-win7-media.ps1 -IsoRoot C:\win7-iso -CertPath C:\aero-test.cer -EnableNoIntegrityChecks:$false
```

- Patch only boot.wim Setup image (index 2):

```powershell
.\scripts\patch-win7-media.ps1 -IsoRoot C:\win7-iso -CertPath C:\aero-test.cer -PatchBootWimIndices 2
```

- Inject the certificate into an additional store (example: `TrustedPeople`):
  
```powershell
.\scripts\patch-win7-media.ps1 `
  -IsoRoot C:\win7-iso `
  -CertPath C:\aero-test.cer `
  -CertStores ROOT,TrustedPublisher,TrustedPeople
```

- Use a `.pfx` certificate bundle:

```powershell
.\scripts\patch-win7-media.ps1 `
  -IsoRoot 'C:\win7-iso' `
  -CertPath 'C:\path\to\aero-test.pfx' `
  -PfxPassword 'password-here'
```

Notes:

- `.pem` files may contain multiple `BEGIN CERTIFICATE` blocks; the script injects **all** of them.
- `.pfx` files may contain multiple certificates; the script injects **all unique thumbprints** found.
- `-PfxPassword` is only required if the `.pfx` is password-protected.
- Customize which LocalMachine certificate stores are populated (default is `ROOT,TrustedPublisher`):

```powershell
.\scripts\patch-win7-media.ps1 `
  -IsoRoot 'C:\win7-iso' `
  -CertPath 'C:\path\to\aero-test.cer' `
  -CertStores ROOT,TrustedPublisher,TrustedPeople
```

---

## What gets patched

### BCD stores on the extracted ISO folder

Patched via `bcdedit /store`:

- `boot\BCD` (BIOS/CSM)
- `efi\microsoft\boot\BCD` (UEFI)

On `{default}`:

- `testsigning on`
- `nointegritychecks on` (optional; defaults to on)

If a given BCD store does not contain `{default}`, the script falls back to patching any compatible entries it finds via `bcdedit /enum all` (typically the `Windows Boot Loader` entries).

### Offline registry hives inside WIM images

For each selected mounted WIM index, the script:

1. Loads the offline `SOFTWARE` hive via `reg load`.
2. Uses Windows CryptoAPI (`crypt32.dll`) against the loaded hive to add the certificate to:
   - `ROOT`
   - `TrustedPublisher`
   - (optional) `TrustedPeople` (if specified via `-CertStores`)
3. Unloads the hive via `reg unload`.

### BCD-Template inside install.wim

Patched via `bcdedit /store`:

- `<MountDir>\Windows\System32\Config\BCD-Template`

On `{default}`:

- `testsigning on`
- `nointegritychecks on` (optional; defaults to on)

---

## Verification

From the patched ISO folder on the host:

```powershell
bcdedit /store 'C:\win7-iso\boot\BCD' /enum {default}
bcdedit /store 'C:\win7-iso\efi\microsoft\boot\BCD' /enum {default}
```

In WinPE/Setup:

- Look for the “Test Mode” watermark, or run:

```cmd
bcdedit /enum {current}
certutil -store Root
certutil -store TrustedPublisher
```

In the installed OS, the certificate should appear in both stores as well.

---

## Notes / Safety

- The script **modifies files in place**. Work on a copy of your extracted ISO directory if you want to preserve the original.
- If a run is interrupted, you may need to clean up stuck WIM mounts manually (e.g. `dism /Cleanup-Wim`).
