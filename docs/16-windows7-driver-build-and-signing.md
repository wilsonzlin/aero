# 16 - Windows 7 Driver Build, Cataloging, Signing, and Installation

## Overview

This document collects practical notes for building, cataloging, and test-signing Windows drivers intended to run on Windows 7 SP1, and documents the CI scripts used to produce installable artifacts.

End-to-end, Aero’s Windows 7 driver pipeline is:

1. Build driver binaries (`.sys`) for **x86** and **x64**
2. Stage a driver package (INF + SYS + any coinstallers)
3. Generate catalogs (`.cat`) with `Inf2Cat`
4. Test-sign catalogs/binaries for development/CI
5. Install drivers on Windows 7 (post-install or during Windows Setup)

The repo provides PowerShell entrypoints that CI and local builds can share:

- `ci/install-wdk.ps1` → locate/install toolchain components and write `out/toolchain.json`
- `ci/validate-toolchain.ps1` → smoke-test that `Inf2Cat /os:7_X86,7_X64` works (catches runner/toolchain regressions early)
- `ci/build-drivers.ps1` → build binaries into `out/drivers/`
- `ci/make-catalogs.ps1` → stage packages + run `Inf2Cat` into `out/packages/`
- `ci/sign-drivers.ps1` → create a test cert + sign `.sys`/`.dll`/`.cat` under `out/packages/`
- `ci/package-drivers.ps1` → create `.zip` bundles and an optional `.iso` for “Load driver” installs

These scripts are orchestrated in CI by:

- `.github/workflows/drivers-win7.yml` (**canonical** PR/push workflow; builds + catalogs + test-signs + packages, then uploads `out/artifacts/` as the `win7-drivers` artifact)
- `.github/workflows/release-drivers-win7.yml` (tagged releases; publishes the packaged artifacts to GitHub Releases)

---

## Supported OS targets

- **Windows 7 SP1 x86 (32-bit)**
- **Windows 7 SP1 x64 (64-bit)**

Notes:

- Windows 7 x64 enforces kernel-mode signatures unless the machine is configured for **test signing**.
- Windows Server 2008 R2 shares the same kernel line (NT 6.1) and generally behaves the same for INF targeting/signature enforcement.

---

## Toolchain choice (CI and local)

### Why we validate the toolchain

Catalog generation is performed with `Inf2Cat.exe`. For Windows 7 we specifically need the OS tokens:

```
Inf2Cat /os:7_X86,7_X64
```

Not every Windows Kits / WDK release has historically accepted older `/os:` tokens, and CI runner images can change over time. A failing catalog-generation step is easy to miss until late in the build pipeline, so we validate it explicitly.

### Pinned Windows Kits version

CI pins the Windows Kits toolchain to:

- **Windows Kits 10.0.22621.0** (Windows 11 / Windows 10 22H2-era toolset)

The pin is implemented in `ci/install-wdk.ps1` (which installs the Windows SDK/WDK via `winget` on CI if needed) and verified by `ci/validate-toolchain.ps1`.

### Toolchain bootstrap (`ci/install-wdk.ps1`)

`ci/install-wdk.ps1` provisions a predictable toolchain for other scripts by writing:

- `out/toolchain.json`

This JSON includes MSBuild, Inf2Cat, signtool, and (when available) stampinf paths using common property names that other scripts understand (`MSBuild`, `Inf2CatPath`, `SignToolPath`, etc.).

### Toolchain validation (`ci/validate-toolchain.ps1`)

From PowerShell:

```powershell
.\ci\install-wdk.ps1
.\ci\validate-toolchain.ps1
```

The scripts write logs/artifacts under:

- `out/toolchain.json` (resolved tool paths)
- `out/toolchain-validation/` (validation transcript + Inf2Cat output)

### CI workflow

The GitHub Actions workflow `.github/workflows/toolchain-win7-smoke.yml` runs on `windows-latest` and:

1. Resolves/installs the pinned toolchain
2. Prints tool versions (`Inf2Cat`, `signtool`, `stampinf`, `msbuild`)
3. Generates a minimal dummy driver package and runs `Inf2Cat /os:7_X86,7_X64`
4. Uploads the logs as workflow artifacts

---

## Local build instructions (PowerShell)

> These steps assume you are on a Windows host with PowerShell. Running in an elevated PowerShell is recommended (some certificate store operations may require it).

### 1) Bootstrap (and optionally validate) the toolchain

```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass -Force
.\ci\install-wdk.ps1
.\ci\validate-toolchain.ps1
```

### 2) Build driver binaries (`.sys`)

```powershell
.\ci\build-drivers.ps1 -ToolchainJson .\out\toolchain.json
```

Outputs:

- Binaries staged under `out/drivers/<driver>/<arch>/...`
- MSBuild logs under `out/logs/drivers/` (including `.binlog` for deep debugging)

### 3) Stage packages + generate catalogs (`Inf2Cat`)

```powershell
.\ci\make-catalogs.ps1 -ToolchainJson .\out\toolchain.json
```

Outputs:

- Staged packages under `out/packages/<driver>/<arch>/`
- `.cat` files generated inside each package directory

### 4) Test-sign the packages (`signtool`)

Default (max Win7 compatibility):

```powershell
.\ci\sign-drivers.ps1 -ToolchainJson .\out\toolchain.json -Digest sha1
```

Dual signing (SHA-1 first, then append SHA-256):

```powershell
.\ci\sign-drivers.ps1 -ToolchainJson .\out\toolchain.json -DualSign
```

Outputs:

- Signed `*.sys`, `*.dll`, and `*.cat` under `out/packages/**` (configurable via `-InputRoot`)
- Public cert (artifact-safe): `out/certs/aero-test.cer`
- Signing PFX (private key): `out/aero-test.pfx` (kept under `out/`, not `out/certs/`)

The script also verifies signatures:

- `.sys`: `signtool verify /kp /v`
- `.dll`: `signtool verify /v`
- `.cat`: `signtool verify /v`

And imports the public cert into the current user **Trusted Root** and **Trusted Publishers** stores (and will also try LocalMachine stores when allowed) so verification works.

### Stable certificate (optional)

For releases you may want a consistent test certificate across runs. `ci/sign-drivers.ps1` supports this by accepting a PFX via environment variables:

- `AERO_DRIVER_PFX_BASE64`: base64-encoded PFX bytes
- `AERO_DRIVER_PFX_PASSWORD`: PFX password

If both are set, the script uses the provided PFX instead of generating a new self-signed certificate, and still exports the public cert as `out/certs/aero-test.cer`.

### 5) Package artifacts (ZIP + optional ISO)
For maximum compatibility with unpatched Windows 7 when using `-Digest sha1` (or `-DualSign`), the *certificate itself* should also be SHA-1-signed (i.e. its signature algorithm should be `sha1RSA`/`sha1WithRSAEncryption`). The script will refuse to use a SHA-256-signed stable certificate for SHA-1 driver signing unless you pass `-AllowSha2CertFallback`.

### 5) Package artifacts (ZIP + optional ISO)

```powershell
.\ci\package-drivers.ps1 -InputRoot .\out\packages -CertPath .\out\certs\aero-test.cer -OutDir .\out\artifacts
```

Outputs:

- `out/artifacts/AeroVirtIO-Win7-<version>-x86.zip`
- `out/artifacts/AeroVirtIO-Win7-<version>-x64.zip`
- `out/artifacts/AeroVirtIO-Win7-<version>-bundle.zip`
- `out/artifacts/AeroVirtIO-Win7-<version>.iso` (unless `-NoIso` is used)

The packaged artifacts include:

- `aero-test.cer`
- `INSTALL.txt` with the exact commands for test signing + certificate import + `pnputil`

---

## Catalog generation (`Inf2Cat`)

### What the catalog is (and why it matters)

On Windows 7, a PnP driver package is typically considered “signed” when:

- The package contains a `.cat` file referenced by the INF, and
- The `.cat` is digitally signed, and
- The `.cat` contains hashes for every file in the package (INF, SYS, DLL, etc.)

**Any time you change any file in the package, you must regenerate and re-sign the catalog.**

### Required INF metadata

`Inf2Cat` expects certain INF metadata. At minimum, the `[Version]` section should contain:

```ini
[Version]
Signature   = "$WINDOWS NT$"
Class       = System
ClassGuid   = {4D36E97D-E325-11CE-BFC1-08002BE10318}
Provider    = %Aero%
DriverVer   = 01/01/2026,1.0.0.0
CatalogFile = aero-driver.cat
```

Key points:

- **`DriverVer` is required** and affects Windows’ driver ranking/selection.
- **`CatalogFile` must match the generated `.cat` filename** exactly.
  - Per-arch names like `CatalogFile.NTx86 = ...` / `CatalogFile.NTamd64 = ...` are also valid as long as the filenames match.

### Running Inf2Cat manually

Normally `ci/make-catalogs.ps1` runs `Inf2Cat` for you, but for debugging it’s useful to know the raw invocation:

```cmd
Inf2Cat.exe /driver:"C:\path\to\driver-package" /os:7_X86,7_X64 /verbose
```

---

## Test signing model

### Why Aero uses test certificates

For development and CI artifacts we use a **test certificate** because:

- WHQL / Attestation signing is not available for iterative builds
- Development artifacts are intended for controlled environments (dev VMs, test images)
- It keeps the signing workflow self-contained and reproducible

### Enable test signing on Windows 7 x64

On the Windows 7 x64 machine (elevated Command Prompt):

```cmd
bcdedit /set {current} testsigning on
```

Reboot to apply. Verify with:

```cmd
bcdedit /enum {current}
```

### Install the test certificate (Root + TrustedPublisher)

Copy `aero-test.cer` to the Windows 7 machine, then run:

```cmd
certutil -addstore -f Root aero-test.cer
certutil -addstore -f TrustedPublisher aero-test.cer
```

---

## SHA-1 vs SHA-2 (Win7 compatibility)

### Default: SHA-1 for maximum out-of-box compatibility

For the broadest compatibility with “fresh” Windows 7 SP1 installs (especially offline VMs), we default to **SHA-1**:

- `signtool sign /fd sha1 ...`

### File digest (`/fd`) is not the whole story

Authenticode signing has two relevant hash/signature choices:

1. The **file digest** used in the Authenticode signature (`signtool sign /fd sha1|sha256`).
2. The **certificate’s own signature algorithm** (for a self-signed cert, the cert is signed by its own key).

On stock Windows 7 SP1 **without SHA-2 updates** (notably **KB3033929** and **KB4474419**), a common failure mode is:

- the driver/catalog is signed with `/fd sha1`, but
- the signing certificate is **SHA-256-signed**,

and Windows fails to validate the certificate chain because it cannot process SHA-2 signatures in certificates.

### `ci/sign-drivers.ps1` fallback behaviour

Some CI runners refuse creating SHA-1-signed certificates. If SHA-1 certificate creation fails, `ci/sign-drivers.ps1`:

- **fails by default**, or
- continues only if `-AllowSha2CertFallback` is provided, in which case it creates a SHA-256-signed certificate and prints a loud warning that **stock Win7 without KB3033929/KB4474419 may fail**.

### If we ever switch to SHA-256-only

If we sign with SHA-256 (`/fd sha256`) or if the certificate is SHA-256-signed, Windows 7 typically requires SHA-2 support updates such as:

- **KB3033929**
- **KB4474419**

### Optional strategy: dual-signing

If we need both:

- legacy Win7 compatibility (SHA-1), and
- stronger SHA-2 signatures for newer systems,

use `ci/sign-drivers.ps1 -DualSign`, which signs twice:

1. SHA-1 signature first
2. Append SHA-256 signature (`signtool sign /as /fd sha256 ...`)

---

## WDK redistributables (WDF coinstaller)

Some Windows 7-era driver packages (especially KMDF-based ones) may require shipping a WDF coinstaller (`WdfCoInstaller*.dll`). This DLL is a **Microsoft WDK redistributable** with its own license terms.

Policy in this repo:

- CI does **not** include any WDK redistributables by default.
- Drivers that require a WDF coinstaller must declare it in `drivers/<name>/ci-package.json`, and CI must be run with explicit opt-in:

```powershell
.\ci\make-catalogs.ps1 -IncludeWdfCoInstaller
```

See: `docs/16-driver-packaging-and-signing.md` and `docs/13-legal-considerations.md`.

---

## Installing drivers on Windows 7

### Install on an already-installed Windows 7 system (`pnputil`)

On Windows 7, use `pnputil` from an elevated command prompt:

```cmd
pnputil -i -a C:\path\to\driver-package\aero-driver.inf
```

### Windows Setup (“Load driver”)

When installing Windows 7, you can load drivers during Setup:

1. Attach the generated driver ISO (`out/artifacts/...Win7-....iso`) to the VM, or copy the driver folder to removable media.
2. In Setup, click **Load driver**.
3. Browse to the folder containing the `.inf` for the correct architecture (`x86` vs `x64`).

---

## Troubleshooting

### Toolchain validation failures

- Run `.\ci\validate-toolchain.ps1` and inspect `out/toolchain-validation/` for the exact `Inf2Cat` invocation and output.

### Build failures (`ci/build-drivers.ps1` / MSBuild)

- Re-run `.\ci\install-wdk.ps1` and confirm it locates MSBuild.
- Inspect:
  - `out/logs/drivers/<driver>-<arch>.msbuild.log`
  - `out/logs/drivers/<driver>-<arch>.msbuild.binlog`

### `Inf2Cat` / catalog failures

**Missing/invalid `DriverVer`**

- Fix: ensure `[Version]` contains a correctly formatted `DriverVer = MM/DD/YYYY,major.minor.build.revision`.

**“No files were found that could be cataloged”**

- Fix: ensure `ci/make-catalogs.ps1` is running on a fully staged package (INF + SYS present) and that the INF references files that actually exist in the package.

**Hash mismatch at install time**

- Symptom: install fails with “The hash for the file is not present in the specified catalog file”.
- Fix: regenerate catalogs and re-sign after any binary change.

### Signing failures (`ci/sign-drivers.ps1` / `signtool`)

**`New-SelfSignedCertificate` not available**

- Fix: install Windows’ PKI/Certificate tooling (PowerShell PKI cmdlets are expected on typical dev hosts).

**Code 52 on Win7 despite `/fd sha1`**

- Fix checklist:
  1. Confirm you actually produced a SHA-1-signed certificate (the script will refuse by default if it cannot).
  2. If you proceed with `-AllowSha2CertFallback`, install KB3033929/KB4474419 or use an updated Win7 image.

### Installation failures on Windows 7 x64

**Device Manager Code 52 (“Windows cannot verify the digital signature…”)**

- Fix checklist:
  1. Confirm test signing is enabled: `bcdedit /enum {current}` → `testsigning Yes`
  2. Confirm the certificate is installed into both `Root` and `TrustedPublisher`
  3. Confirm correct architecture packages (x86 vs x64)

**Where to look for details**

- `%WINDIR%\inf\setupapi.dev.log` contains detailed driver installation diagnostics (search for the INF name).
