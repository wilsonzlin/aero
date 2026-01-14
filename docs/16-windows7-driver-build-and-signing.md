# 16 - Windows 7 Driver Build, Cataloging, Signing, and Installation

## Overview

This document collects practical notes for building, cataloging, and test-signing Windows drivers intended to run on Windows 7 SP1, and documents the CI scripts used to produce installable artifacts.

End-to-end, Aero’s Windows 7 driver pipeline is:

1. Build driver binaries (`.sys`) for **x86** and **x64**
2. Stage a driver package (INF + SYS + any coinstallers)
3. Generate catalogs (`.cat`) with `Inf2Cat`
4. Test-sign catalogs/binaries for development/CI
5. Bundle signed packages into distributable artifacts (driver bundle ZIP/ISO and/or Guest Tools media)
6. Install drivers on Windows 7 (post-install or during Windows Setup)

The repo provides PowerShell entrypoints that CI and local builds can share:

- `ci/install-wdk.ps1` → locate/install toolchain components and write `out/toolchain.json`
- `ci/validate-toolchain.ps1` → smoke-test that `Inf2Cat /os:7_X86,7_X64` works (catches runner/toolchain regressions early)
- `ci/build-drivers.ps1` → build binaries into `out/drivers/`
- `ci/build-aerogpu-dbgctl.ps1` → build AeroGPU dbgctl helper tool (required by CI packaging manifests that reference it)
- `ci/make-catalogs.ps1` → stage packages + run `Inf2Cat` into `out/packages/`
- `ci/sign-drivers.ps1` → create a test cert + sign `.sys`/`.cat` under `out/packages/` (signed catalogs cover INF-referenced payload files like user-mode DLLs)
- `ci/package-drivers.ps1` → create `.zip` bundles and an optional `.iso` for “Load driver” installs
- `ci/package-guest-tools.ps1` → build Guest Tools media (`aero-guest-tools.iso`/`.zip`) from signed packages using a packager spec (selects a subset of drivers)

These scripts are orchestrated in CI by:

- `.github/workflows/drivers-win7.yml` (**canonical** PR/push workflow; builds + catalogs + test-signs + packages)
  - Driver bundles: `win7-drivers` (from `out/artifacts/`)
  - Raw signed packages + cert: `win7-drivers-signed-packages` (from `out/packages/**` + `out/certs/aero-test.cer`)
  - Guest Tools media: `aero-guest-tools` (via `ci/package-guest-tools.ps1`; ISO/zip/manifest)
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

#### CI driver selection (explicit opt-in via `ci-package.json`)

`ci/build-drivers.ps1` only builds drivers that are explicitly marked as **CI-packaged** by placing a `ci-package.json` manifest at the *driver root* (for example `drivers/windows7/virtio-net/ci-package.json`).

`ci-package.json` is the explicit CI packaging gate: CI discovery starts from `drivers/**/ci-package.json` and only those drivers can flow into `out/drivers/`, `out/packages/`, and the final driver bundle artifacts. (Guest Tools media is further filtered by a separate packager spec; see below.)

The CI discovery rule is:

- Candidate driver roots are discovered from directories under `drivers/` that contain `ci-package.json`.
- Each candidate driver root must contain:
  - a build target: `<dirName>.sln` **or** exactly one `*.vcxproj`
  - at least one `*.inf` somewhere under its tree (excluding `obj/`, `out/`, `build/`, `target/`)
- MakeFileProj/Makefile wrapper projects (legacy WDK `build.exe`) are skipped by default.
  - Pass `-IncludeMakefileProjects` to opt in.
  - For mixed solutions (MSBuild projects + wrapper projects), CI builds only the non-wrapper `*.vcxproj` projects.

To add a new driver to CI packaging:

1. Copy `drivers/_template/ci-package.json` into your driver root as `ci-package.json`
2. Update the `$schema` relative path as needed
3. Replace the `infFiles` placeholder (or remove the `infFiles` key to enable CI auto-discovery of all `*.inf` files under the driver directory).
   - If the driver directory contains multiple INFs, an explicit `infFiles` allowlist is recommended to avoid packaging unrelated variants together.
4. Optionally set `wow64Files` if the x64 package needs specific 32-bit user-mode payload DLLs copied in from the x86 build output (WOW64 components).
   - Ensure WOW64 DLL names do not collide with 64-bit build output names, since WOW64 payloads are copied into the x64 package root.
5. Optionally include extra packaging assets/tools:
   - `additionalFiles` for non-binary files (README/license text, install scripts, extra `.inf` under subdirectories, etc).
   - `toolFiles` for user-mode helper tool binaries (`.exe`) checked into the driver source tree (explicit opt-in; `.exe` is intentionally disallowed in `additionalFiles`).

See also the examples under `drivers/_template/`:

- `ci-package.README.md` (field reference)
- `ci-package.json` (starter template)
- `ci-package.inf-wow64-example.json`
- `ci-package.tools-example.json`
- `ci-package.wdf-example.json`

> Note: CI only builds/stages drivers with `ci-package.json`; drivers without it are treated as dev/test and skipped.
>
> `drivers/win7/virtio/virtio-transport-test/` is a KMDF smoke-test driver and is intentionally **not** CI-packaged (no `ci-package.json`), so it does not ship in CI-produced driver bundles / Guest Tools artifacts. Its `virtio-transport-test.inf` intentionally binds a **non-contract** virtio PCI HWID (`PCI\VEN_1AF4&DEV_1040`) so it cannot steal binding from production virtio devices if you install it manually alongside other drivers.
> The virtio-input driver under `drivers/windows7/virtio-input/` is revision-gated to Aero contract v1 (`...&REV_01`).
> INF matching policy for virtio-input keyboard/mouse:
> - Canonical INF: `inf/aero_virtio_input.inf`
>   - Binds the subsystem-qualified keyboard/mouse contract v1 HWIDs (`SUBSYS_0010` / `SUBSYS_0011`, both `&REV_01`) for distinct
>     Device Manager names.
>   - Also includes the strict revision-gated generic fallback HWID (no `SUBSYS`) for environments where subsystem IDs are not
>     exposed/recognized: `PCI\VEN_1AF4&DEV_1052&REV_01` (Device Manager name: **Aero VirtIO Input Device**).
> - Optional legacy alias INF (disabled by default): `inf/virtio-input.inf.disabled` → rename to `inf/virtio-input.inf`
>   - Exists for compatibility with workflows/tools that still reference `virtio-input.inf`.
>   - Filename-only alias: from `[Version]` onward, it must remain byte-for-byte identical to the canonical INF (banner/comments above
>     `[Version]` may differ). See `drivers/windows7/virtio-input/scripts/check-inf-alias.py`.
>   - Enabling the alias does **not** change HWID matching behavior (it is a pure basename compatibility shim).
>
> Tablet devices bind via the separate `inf/aero_virtio_tablet.inf` (`SUBSYS_00121AF4`); that INF is more specific and wins
> over the generic fallback match when both packages are present and the tablet subsystem ID matches.
>
> Avoid shipping/installing both basenames at once (they overlap and can cause confusing driver selection). Prefer explicit
> `ci-package.json` `infFiles` allowlists so only one of the two INF basenames is packaged.

```powershell
.\ci\build-drivers.ps1 -ToolchainJson .\out\toolchain.json
```

Outputs:

- Binaries staged under `out/drivers/<driver>/<arch>/...`
- MSBuild logs under `out/logs/drivers/` (including `.binlog` for deep debugging)

### 3) Build AeroGPU dbgctl (required for CI-style packaging)

Some driver packages (notably `drivers/aerogpu`) ship auxiliary tooling alongside the driver package (for example, `aerogpu_dbgctl.exe`).

These tools should be treated as build outputs: build them, then copy them into `out/drivers/<driver>/<arch>/...` so `ci/make-catalogs.ps1` will stage them into `out/packages/**`. Drivers that require such tools can enforce their presence via `requiredBuildOutputFiles` in `ci-package.json`.

`ci/build-aerogpu-dbgctl.ps1` verifies the dbgctl build output at:

- `drivers/aerogpu/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe`

and (when `out/drivers/aerogpu/<arch>/` exists, i.e. after `ci/build-drivers.ps1`) copies it into:

- `out/drivers/aerogpu/x86/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe`
- `out/drivers/aerogpu/x64/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe`

When these signed packages are packaged into Guest Tools, dbgctl is shipped inside the AeroGPU driver directory at:

- `drivers/amd64/aerogpu/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe`
- `drivers/x86/aerogpu/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe`
  - Some Guest Tools builds also include a convenience copy in the optional top-level `tools/` payload (when present):
    - `tools/aerogpu_dbgctl.exe`
    - `tools/<arch>/aerogpu_dbgctl.exe`

Bitness policy:

- `aerogpu_dbgctl.exe` is intentionally built/shipped as an **x86 (32-bit)** tool.
- For the x64 driver/package tree we still ship the same x86 binary and run it under **WOW64**.
- CI enforces this by inspecting the PE header and requiring `IMAGE_FILE_MACHINE_I386 (0x014c)`.

Build it before catalog generation:

```powershell
.\ci\build-aerogpu-dbgctl.ps1 -ToolchainJson .\out\toolchain.json
```

### 4) Stage packages + generate catalogs (`Inf2Cat`)

`ci/make-catalogs.ps1` stages driver packages under `out/packages/` by combining:

- packaging assets from `drivers/<driver>/` (INF files, optional coinstallers, etc), and
- built binaries from `out/drivers/<driver>/<arch>/`,

then runs `Inf2Cat` in each staged package directory.

Only drivers that opt into CI packaging via `drivers/<driver>/ci-package.json` are staged. The manifest can also constrain which INF files are included via `infFiles` (recommended when a driver directory contains multiple INFs).

```powershell
.\ci\make-catalogs.ps1 -ToolchainJson .\out\toolchain.json
```

Outputs:

- Staged packages under `out/packages/<driver>/<arch>/`
- `.cat` files generated inside each package directory

### 5) Test-sign the packages (`signtool`)

Default (max Win7 compatibility):

```powershell
.\ci\sign-drivers.ps1 -ToolchainJson .\out\toolchain.json -Digest sha1
```

Dual signing (SHA-1 first, then append SHA-256):

```powershell
.\ci\sign-drivers.ps1 -ToolchainJson .\out\toolchain.json -DualSign
```

Outputs:

- Signed `*.sys` and `*.cat` under `out/packages/**` (configurable via `-InputRoot`)
  - Note: `Inf2Cat` catalogs hash all INF-referenced files in the package (INF, SYS, DLL, etc). Windows PnP validates package contents against those hashes via the signed catalog, so Authenticode-signing user-mode DLLs individually is optional.
- Public cert (artifact-safe): `out/certs/aero-test.cer`
- Signing PFX (private key): `out/aero-test.pfx` (kept under `out/`, not `out/certs/`)

The script also verifies signatures:

- `.sys`: `signtool verify /kp /v`
- `.cat`: `signtool verify /v`

And imports the public cert into the current user **Trusted Root** and **Trusted Publishers** stores (and will also try LocalMachine stores when allowed) so verification works.

### Stable certificate (optional)

For releases you may want a consistent test certificate across runs. `ci/sign-drivers.ps1` supports this by accepting a PFX via environment variables:

- `AERO_DRIVER_PFX_BASE64`: base64-encoded PFX bytes
- `AERO_DRIVER_PFX_PASSWORD`: PFX password

If both are set, the script uses the provided PFX instead of generating a new self-signed certificate, and still exports the public cert as `out/certs/aero-test.cer`.

### 6) Package artifacts (ZIP + optional ISO)

Compatibility note: for maximum compatibility with unpatched Windows 7 when using `-Digest sha1` (or `-DualSign`), the *certificate itself* should also be SHA-1-signed (i.e. its signature algorithm should be `sha1RSA`/`sha1WithRSAEncryption`). The script will refuse to use a SHA-256-signed stable certificate for SHA-1 driver signing unless you pass `-AllowSha2CertFallback`.

```powershell
.\ci\package-drivers.ps1 -InputRoot .\out\packages -CertPath .\out\certs\aero-test.cer -OutDir .\out\artifacts
```

Signing policy:

- Default: `-SigningPolicy test`
  - Requires `-CertPath` (defaults to `out/certs/aero-test.cer`).
  - Bundles `aero-test.cer` into the ZIP/ISO roots.
  - `INSTALL.txt` includes **test signing** and **certificate import** steps.
- For production/WHQL-signed drivers, use `-SigningPolicy production` (or `none`):
  - Does **not** require `-CertPath`.
  - Does **not** bundle any certificate files.
  - `INSTALL.txt` omits test-signing/certificate import steps.

Outputs:

- `out/artifacts/AeroVirtIO-Win7-<version>-x86.zip`
- `out/artifacts/AeroVirtIO-Win7-<version>-x64.zip`
- `out/artifacts/AeroVirtIO-Win7-<version>-bundle.zip`
- `out/artifacts/AeroVirtIO-Win7-<version>.iso` (unless `-NoIso` is used; requires Rust/cargo for deterministic builds by default; use `-LegacyIso` for Windows IMAPI2 which is **not** deterministic)
- `out/artifacts/AeroVirtIO-Win7-<version>-fat.vhd` (optional; when `-MakeFatImage` or `AERO_MAKE_FAT_IMAGE=1`; requires Windows + Administrator privileges; skipped unless `-FatImageStrict`)

Integrity manifests (default; disable with `-NoManifest`):

- `out/artifacts/AeroVirtIO-Win7-<version>-x86.manifest.json`
- `out/artifacts/AeroVirtIO-Win7-<version>-x64.manifest.json`
- `out/artifacts/AeroVirtIO-Win7-<version>-bundle.manifest.json`
- `out/artifacts/AeroVirtIO-Win7-<version>.manifest.json` (when ISO is produced)
- `out/artifacts/AeroVirtIO-Win7-<version>-fat.manifest.json` (when FAT VHD is produced)

Each `*.manifest.json` includes the artifact's `sha256`/`size`, the packaging `version`,
`signing_policy`, and (when present) `package.build_id` (defaults to the HEAD commit SHA), plus a
stable per-file hash list for mixed-media detection.

The packaged artifacts include:

- `INSTALL.txt` with the exact commands for `pnputil` installs (and, for `-SigningPolicy test`, test signing + certificate import)
- `aero-test.cer` *(SigningPolicy=test only)*

These driver bundle artifacts include **all** staged CI-packaged drivers under `out/packages/`. If you opt a dev/test driver into CI packaging, it will appear in the bundle artifacts; ensure its INF does not bind production HWIDs so it cannot steal device binding when multiple driver packages are present.

### 7) Package Guest Tools media (ISO/ZIP) (optional)

`ci/package-guest-tools.ps1` consumes the signed driver packages under `out/packages/` and produces the Guest Tools ISO/zip. Unlike the driver bundle ZIP/ISO, Guest Tools includes only the drivers selected by a packager spec (`-SpecPath`):

- **CI/release workflows:** `tools/packaging/specs/win7-signed.json`
- **Local default (when `-SpecPath` is omitted):** `tools/packaging/specs/win7-aero-guest-tools.json` (stricter HWID validation)

This means a driver can be CI-packaged (built + cataloged + signed) and appear in the driver bundle artifacts, but still be omitted from Guest Tools if it is not selected by the spec.

```powershell
.\ci\package-guest-tools.ps1 -InputRoot .\out\packages -CertPath .\out\certs\aero-test.cer -OutDir .\out\artifacts -SpecPath .\tools\packaging\specs\win7-signed.json
```

Outputs:

- `out/artifacts/aero-guest-tools.iso`
- `out/artifacts/aero-guest-tools.zip`
- `out/artifacts/manifest.json`
- `out/artifacts/aero-guest-tools.manifest.json` (copy of `manifest.json`, used by CI/release asset publishing)

---

## Catalog generation (`Inf2Cat`)

### What the catalog is (and why it matters)

On Windows 7, a PnP driver package is typically considered “signed” when:

- The package contains a `.cat` file referenced by the INF, and
- The `.cat` is digitally signed, and
- The `.cat` contains hashes for every file in the package (INF, SYS, DLL, etc.)

**Any time you change any file in the package, you must regenerate and re-sign the catalog.**

Note: whether `Inf2Cat` includes *unreferenced* extra files found under the package directory tree in the catalog is a toolchain detail. CI’s Win7 toolchain smoke test (`ci/validate-toolchain.ps1`) prints `INF2CAT_UNREFERENCED_FILE_HASHED=0|1` so toolchain updates don’t silently change this behaviour. In practice, treat staged package directories as immutable after catalog generation, and stage any auxiliary tools (e.g. dbgctl under `tools/`) before running `ci/make-catalogs.ps1`.

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
- Drivers that require a WDF coinstaller must declare it in `drivers/<driver>/ci-package.json`, and CI must be run with explicit opt-in:

```powershell
.\ci\build-aerogpu-dbgctl.ps1 -ToolchainJson .\out\toolchain.json
.\ci\make-catalogs.ps1 -ToolchainJson .\out\toolchain.json -IncludeWdfCoInstaller
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
