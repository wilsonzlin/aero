# Windows tools

## `patch-win7-media.ps1`

Patches extracted Windows 7 install media to support **test-signed drivers** by:

- Enabling `testsigning` (and optionally `nointegritychecks`) in installer BCD stores
- Mounting and servicing `sources\boot.wim` and `sources\install.wim`
- Offline-injecting a signing certificate into the image `ROOT` and `TrustedPublisher` stores
- Validating the offline `SOFTWARE` hive contains `Microsoft\SystemCertificates\{ROOT,TrustedPublisher}\Certificates\<thumbprint>\Blob`
- (Optional) Injecting driver packages from a directory containing `.inf` files
- Updating the offline `BCD-Template` inside each selected `install.wim` image

For BCD hive internals (element IDs, well-known object GUIDs, and how to locate OS loader
entries offline), see: `docs/win7-bcd-offline-patching.md`.
- (Optional) Patching the nested WinRE image (`Windows\System32\Recovery\winre.wim`) inside each `install.wim` index

### Prerequisites

- Windows PowerShell **5.1+**
- Run from an **elevated** PowerShell prompt (Run as Administrator)
- `dism.exe`, `bcdedit.exe`, `attrib.exe` available (standard on Windows)
- `win-offline-cert-injector.exe` (build from `tools/win-offline-cert-injector` or place it in `PATH`)
- A **writable** extracted Windows 7 ISO directory:
  - Must contain `sources\boot.wim` and `sources\install.wim`
  - Recommended: copy ISO contents to a local NTFS directory (don’t patch directly on read-only media)
- A certificate file (`.cer`) used to sign your test drivers
  - `patch-win7-media.ps1` will clear the filesystem `Read-only` attribute on `boot.wim`/`install.wim` if present, but it cannot patch files on truly read-only media.

Build the offline injector once:

```powershell
cd tools\win-offline-cert-injector
cargo build --release
```

### Usage examples

Patch only `boot.wim` index 2 and `install.wim` index 4:

```powershell
.\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\iso\win7sp1 `
  -CertPath  C:\certs\driver-test.cer `
  -BootWimIndices 2 `
  -InstallWimIndices "4"
```

Patch *all* `install.wim` indices (default) and both `boot.wim` indices (default), including driver injection:

```powershell
.\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\iso\win7sp1 `
  -CertPath  C:\certs\driver-test.cer `
  -DriversPath C:\drivers\win7
```

Inject the certificate into additional stores (`TrustedPeople` + `CA`):

```powershell
.\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\iso\win7sp1 `
  -CertPath  C:\certs\driver-test.cer `
  -DriversPath C:\drivers\win7 `
  -CertStores ROOT,CA,TrustedPublisher,TrustedPeople
```

Patch with `nointegritychecks` enabled as well:

```powershell
.\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\iso\win7sp1 `
  -CertPath  C:\certs\driver-test.cer `
  -DriversPath C:\drivers\win7 `
  -EnableNoIntegrityChecks
```

Also patch the nested WinRE image (`winre.wim`) inside each selected `install.wim` index:

```powershell
.\tools\windows\patch-win7-media.ps1 `
  -MediaRoot C:\iso\win7sp1 `
  -CertPath  C:\certs\driver-test.cer `
  -DriversPath C:\drivers\win7 `
  -PatchNestedWinRE
```

### Verification hints

The script prints `bcdedit /store ... /enum {default}` commands you can run to confirm the flags were applied for:

- Media stores:
  - `boot\BCD`
  - `efi\microsoft\boot\bcd` (if present)
- Offline template inside each `install.wim` index:
  - `Windows\System32\Config\BCD-Template`

If `{default}` is not present in a given store (common for some `BCD-Template` variants), the script falls back to `bcdedit /enum all` and attempts to set the flags on any GUID entries that accept them (avoids relying on locale-specific `bcdedit` output parsing).
