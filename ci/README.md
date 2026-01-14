# Windows 7 driver CI scripts

## `ci/install-wdk.ps1`

Provisions the Windows driver build toolchain for CI/local builds:

- `msbuild.exe` (Visual Studio Build Tools / MSBuild)
- `Inf2Cat.exe` (WDK; validated to support `/os:7_X86,7_X64`)
- `signtool.exe` (Windows SDK)
- `stampinf.exe` (optional but recommended)

Outputs:

- Writes `out/toolchain.json` (absolute paths + provenance) for use by other scripts.
- In GitHub Actions, also exports tool paths via `$GITHUB_OUTPUT`, `$GITHUB_ENV`, and `$GITHUB_PATH`.
- If `WDK_DOWNLOAD_CACHE` is set, `winget` downloads are directed there (so CI can cache installers across runs).
- If `winget` is unavailable, the script will try a best-effort Chocolatey fallback (`choco install windows-driver-kit`).
- Installing the SDK/WDK requires Administrator privileges; if the tools are missing and the shell is not elevated, the script fails with remediation.

Example local usage:

```powershell
pwsh -File ci/install-wdk.ps1
pwsh -File ci/build-drivers.ps1 -ToolchainJson out/toolchain.json
# Required when drivers/aerogpu/ci-package.json declares dbgctl via `requiredBuildOutputFiles`.
pwsh -File ci/build-aerogpu-dbgctl.ps1 -ToolchainJson out/toolchain.json
pwsh -File ci/make-catalogs.ps1 -ToolchainJson out/toolchain.json
pwsh -File ci/sign-drivers.ps1 -ToolchainJson out/toolchain.json
pwsh -File ci/package-drivers.ps1 -MakeFatImage
pwsh -File ci/package-guest-tools.ps1
```

## `ci/build-drivers.ps1`

Builds driver projects under `drivers/` for the requested platforms/configuration using `msbuild.exe`.

## `ci/build-aerogpu-dbgctl.ps1`

Builds the standalone Win7 AeroGPU debug/control tool (`aerogpu_dbgctl.exe`) using `cl.exe` (not MSBuild).

- Build script: `drivers/aerogpu/tools/win7_dbgctl/build_vs2010.cmd`
- Output (in-tree): `drivers/aerogpu/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe`

Bitness policy (important):

- `aerogpu_dbgctl.exe` is intentionally shipped as an **x86 (32-bit)** PE executable.
- The same x86 binary is staged into both the x86 and x64 driver outputs under:
  - `out/drivers/aerogpu/<arch>/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe`
  - On Windows x64 it runs under **WOW64**.
- `ci/build-aerogpu-dbgctl.ps1` imports an x86 Visual Studio developer environment (VsDevCmd/vcvarsall) and
  validates the produced binary is `IMAGE_FILE_MACHINE_I386 (0x014c)`, failing fast if it is x64 (0x8664) or unknown.

In CI, this script is run after `ci/build-drivers.ps1` so it can also copy the built tool into
`out/drivers/aerogpu/<arch>/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe`, allowing downstream catalog/sign/package steps to
ship it inside driver packages and Guest Tools media.

This is required when `drivers/aerogpu/ci-package.json` lists `tools/win7_dbgctl/bin/aerogpu_dbgctl.exe` under
`requiredBuildOutputFiles` — `ci/make-catalogs.ps1` will fail packaging if the tool was not staged into
the per-arch build output directories.

### CI packaging gate (`ci-package.json`)

For CI determinism (and to avoid accidentally shipping dev/test drivers), `ci/build-drivers.ps1` only
builds drivers that are explicitly opted into CI packaging by placing `ci-package.json` at the driver
root.

See:

  - `ci/driver-package.schema.json`
  - Template manifests under `drivers/_template/`:
    - `ci-package.README.md` (field reference)
    - `ci-package.json` (starter template; replace `infFiles` placeholder `REPLACE_ME.inf`, or remove `infFiles` to enable CI auto-discovery)
    - `ci-package.inf-wow64-example.json` (INF selection + WOW64 payload DLL example)
    - `ci-package.tools-example.json` (INF selection + user-mode tool(s) via `toolFiles`)
    - `ci-package.wdf-example.json` (WDF coinstaller example)

### Legacy WDK BUILD / NMake wrapper projects

Some legacy driver projects are Visual Studio "Makefile" projects (`<Keyword>MakeFileProj</Keyword>` /
`<ConfigurationType>Makefile</ConfigurationType>`) that invoke classic WinDDK 7600 `build.exe`.
The modern toolchain installed by `ci/install-wdk.ps1` does not provide `build.exe`, so CI **skips**
these projects by default.

If a solution contains a *mix* of wrapper projects and real MSBuild projects, CI builds only the
non-wrapper `*.vcxproj` projects (so `msbuild <solution>.sln` never attempts to invoke `build.exe`).

To opt in locally (when you have `build.exe` available in your environment):

```powershell
pwsh -File ci/build-drivers.ps1 -ToolchainJson out/toolchain.json -IncludeMakefileProjects
```

## `ci/stamp-infs.ps1`

Stamps `DriverVer` in staged `.inf` files (in-place) using WDK `stampinf.exe`.

Defaults (when overrides are not provided):

- **Date**: the HEAD commit date (`git show -s --format=%cI HEAD`), clamped to build time so it is never in the future.
- **Version**: derived from the nearest `vMAJOR.MINOR.PATCH` git tag + commit distance:
  - `DriverVer` version: `MAJOR.MINOR.PATCH.<distance>`
  - Package version (for logs/artifact naming): `MAJOR.MINOR.PATCH+<distance>.g<shortsha>`

This script only stamps INFs inside the provided staging directory.
If `-ToolchainJson` is provided, it will use `StampInfExe` from that manifest (when present) for deterministic WDK tool resolution.

## `ci/make-catalogs.ps1`

Runs `ci/stamp-infs.ps1` **before** calling `Inf2Cat.exe`, because catalog hashes include the INF contents.

Note: `Inf2Cat` catalog membership is toolchain-dependent (see the `ci/validate-toolchain.ps1` smoke test note below). If you ship auxiliary tools or other extra files inside the staged driver package directory (for example under `tools/`), stage them **before** this step so the package contents used for catalog generation are final.

Only drivers that include `drivers/<driver>/ci-package.json` are staged into `out/packages/`. This is the
explicit opt-in gate that keeps CI driver bundles from accidentally including dev/test drivers.

### Staged package sanitation (`out/packages/**`)

After staging each driver+arch package directory, `ci/make-catalogs.ps1` performs a sanitation pass over the
staged contents to keep artifacts redistributable and stable:

- Deletes common debug/intermediate outputs (`.pdb`, `.obj`, `.lib`, `.log`, etc).
- Deletes common source/project files (`.c/.cpp/.h`, `.vcxproj`, `.sln`, etc).
- Deletes OS metadata files (`Thumbs.db`, `desktop.ini`, `.DS_Store`).
- **Fails the build** if any likely secret/private key material is present anywhere in the staged package
  (`.pfx`, `.pvk`, `.snk`, `.pem`, `.key`, etc). Signing keys must never be shipped inside driver packages.

If you need to ship extra (non-binary) files intentionally, add them under the driver source tree and include
them via `drivers/<driver>/ci-package.json` → `additionalFiles`.

If you need to ship user-mode helper tool binaries (`.exe`) alongside the driver package, use
`drivers/<driver>/ci-package.json` → `toolFiles` (explicit opt-in; `.exe` remains disallowed in `additionalFiles`).

If a legitimate redistributable artifact is removed by sanitation, update the allowlist in
`ci/make-catalogs.ps1` in a follow-up (we prefer explicit allowlist additions over copying all build outputs
verbatim).

Environment variables:

- `AERO_STAMP_INFS`: `0|false|no|off` disables stamping (default is enabled).
- `AERO_INF2CAT_OS`: overrides the `/os:` list passed to `Inf2Cat.exe` (default: `7_X64,7_X86`).

WDK redistributables (WDF coinstaller):

- **Default:** CI does not copy any WDK redistributable binaries.
- To include `WdfCoInstaller*.dll`, a driver must declare `wdfCoInstaller` in `drivers/<driver>/ci-package.json` and `ci/make-catalogs.ps1` must be run with `-IncludeWdfCoInstaller` (or `-IncludeWdkRedist WdfCoInstaller`).
- The script will fail if it detects `WdfCoInstaller*.dll` checked into the repo under `drivers/` (to prevent accidental redistribution).

Other per-driver packaging manifest features:

Note: CI trims string values in `ci-package.json` and treats paths case-insensitively when checking for duplicate entries (also treating `\` and `/` as equivalent separators).

- `infFiles`: explicitly select which INF(s) are staged for a driver (useful when a driver ships multiple INFs with overlapping HWIDs and should not be packaged as a single combined folder). If present, the list must be non-empty. Paths are relative to the driver directory (no absolute paths / drive letters / UNC roots) and should not contain `..` segments. Build-output `.inf` files are ignored/removed; only selected (or auto-discovered-from-source) INF(s) (plus any INF explicitly staged via `additionalFiles`) are allowed in the staged package.
- `wow64Files`: for x64 packages that need 32-bit user-mode components, copy specific `.dll` file names (no paths) from the x86 build output into the x64 staging directory *before* stamping INFs + running Inf2Cat. Requires x86 build outputs to be present even when generating/staging only x64 packages. Ensure the WOW64 DLL names do not collide with 64-bit build outputs (use distinct names such as a `_x64` suffix for 64-bit DLLs).
- `additionalFiles`: copy extra non-binary files from the driver directory into the staged package (relative paths are preserved). Intended for README/license text, install scripts, and extra `.inf` files under subdirectories. Paths are relative to the driver directory (no absolute paths / drive letters / UNC roots) and should not contain `..` segments. CI refuses common binary extensions via this mechanism (`.sys`, `.dll`, `.exe`, `.cat`, `.msi`, `.cab`) and refuses likely secret/private key material (`.pfx`, `.pvk`, `.snk`, `.pem`, `.key`, `.p8`, `.pk8`, `.der`, `.csr`, etc).
- `toolFiles`: explicitly include `.exe` helper tools from the driver source tree alongside the staged package without relaxing the `additionalFiles` non-binary policy. Paths are relative to the driver directory (no absolute paths / drive letters / UNC roots) and should not contain `..` segments.
- `requiredBuildOutputFiles`: list of expected build outputs (paths relative to `out/drivers/<driver>/<arch>/`) that must exist after building. CI fails early if any required path is missing, which helps catch accidentally-disabled build steps (for example, a missing in-guest debug utility).

See: `docs/16-driver-packaging-and-signing.md`.

## `ci/sign-drivers.ps1`

Test-signs staged driver packages under `out/packages/` (or `-InputRoot`) using `signtool.exe`.

When signing the CI packages layout (`out/packages/**`), the script refuses to sign any driver package
that is not explicitly opted into CI packaging via `drivers/<driver>/ci-package.json` (defense-in-depth).

CI signs:

- `*.sys` (kernel-mode drivers)
- `*.cat` (catalogs)

Note: `Inf2Cat` catalogs include hashes for INF-referenced files (INF, SYS, DLL, etc). Whether `Inf2Cat` also hashes **extra files present under the package directory tree** (even when they are not referenced by the INF) is a toolchain detail we validate in CI.

CI’s Win7 toolchain smoke test (`ci/validate-toolchain.ps1`) includes a minimal experiment that adds an unreferenced file under `tools/` and checks whether its name appears in the generated `.cat` (via `certutil -dump`, with a raw-byte fallback). The logs include a stable summary line:

- `INF2CAT_UNREFERENCED_FILE_HASHED=0`: unreferenced extra files are not cataloged.
- `INF2CAT_UNREFERENCED_FILE_HASHED=1`: unreferenced extra files are cataloged.

Practical rule: treat staged package directories as immutable after `ci/make-catalogs.ps1` runs. For packaged helper tools (for example `aerogpu_dbgctl.exe` staged as `tools/win7_dbgctl/bin/aerogpu_dbgctl.exe`), build/copy them into the staging directory *before* catalog generation.

And verifies:

- `.sys`: `signtool verify /kp /v`
- `.cat`: `signtool verify /v`

## `ci/package-drivers.ps1`

Packages signed driver staging folders from `out/packages/` into release artifacts under `out/artifacts/`.

This step refuses to package any driver that is not explicitly opted into CI packaging via
`drivers/<driver>/ci-package.json` (defense-in-depth against stale/stray packages under `out/packages/`).

### OutDir safety check

`ci/package-drivers.ps1` deletes a staging directory at `$OutDir/_staging`. To prevent accidental
deletion when `-OutDir` is misconfigured, the script **refuses** to run when `-OutDir` points outside
`<repo>/out` (or when it is the repo root / drive root).

To intentionally write artifacts outside `<repo>/out`, pass `-AllowUnsafeOutDir`.

### Deterministic ISO creation (cross-platform)

`ci/package-drivers.ps1` builds the bundle ISO using the deterministic Rust
ISO writer (`tools/packaging/aero_packager`, binary: `aero_iso`). This makes the produced
`AeroVirtIO-Win7-*.iso` **bit-identical** across runs/hosts as long as the staged bundle directory
contents (and `SOURCE_DATE_EPOCH`, if set) are identical.

- When `cargo` is available, ISO creation is deterministic and works on Windows, Linux, and macOS (no IMAPI2 required).
- On Windows only, if `cargo` is missing (or `-LegacyIso` is passed), it falls back to the legacy IMAPI2 ISO builder (**not deterministic**).
- In CI, we require `cargo` to avoid accidentally producing non-deterministic ISO artifacts.
- Use `-NoIso` to skip ISO creation.
- Use `-LegacyIso` to force the legacy Windows IMAPI2 path (not deterministic).

Notes:

- `ci/package-drivers.ps1 -DeterminismSelfTest` checks that repeated runs produce identical `*-x86.zip`, `*-x64.zip`, and `*-bundle.zip` artifacts (and their `*.manifest.json` files), and identical `*.iso` artifacts (and its manifest) when ISO creation is enabled.
- The legacy helper `ci/lib/New-IsoFile.ps1` also prefers `cargo`/`aero_iso` when available, falling back to IMAPI2 on Windows.

### Signing policy

- `-SigningPolicy test` (default):
  - Requires `-CertPath` (`out/certs/aero-test.cer` by default).
  - Bundles `aero-test.cer` into the ZIP/ISO roots.
  - `INSTALL.txt` includes certificate installation + test signing instructions.
- `-SigningPolicy production` (or `none`):
  - Does **not** require `-CertPath`.
  - Does **not** bundle `aero-test.cer`.
  - `INSTALL.txt` omits certificate/test signing steps and notes that drivers are expected to be production/WHQL signed.
### Driver directory naming + collision detection

`ci/package-drivers.ps1` stages drivers into `drivers/<driverName>/<arch>/...` inside the output zip/iso.

When staging from nested input layouts like `out/packages/<group>/<driver>/<arch>/...`, it uses a heuristic
(`Get-DriverNameFromRelativeSegments`) to pick a leaf `<driverName>`. If two *different* source driver
packages map to the same destination folder, the script now **fails fast** to avoid silently merging /
overwriting packages.

To disambiguate (or to maintain stable output names across input layouts), provide an explicit mapping:

#### `-DriverNameMapJson`

Path to a JSON object mapping input driver identifiers to desired output driver directory names.

- **Keys** (case-insensitive) may be either:
  - a `driverRel` (relative path under `out/packages`, excluding the arch segment), e.g. `windows7/virtio/blk`
  - a leaf driver folder name (e.g. `blk`)
- **Values** are the desired output directory name under `drivers/<value>/<arch>/...`.

Example:

```json
{
  "windows7/virtio/blk": "virtio-blk",
  "windows7/virtio/net": "virtio-net",
  "blk": "virtio-blk"
}
```

Self-test (demonstrates collision detection + mapping override):

```powershell
pwsh -File ci/package-drivers.ps1 -SelfTest
```

Artifacts (typical):

- `AeroVirtIO-Win7-<version>-x86.zip`
- `AeroVirtIO-Win7-<version>-x64.zip`
- `AeroVirtIO-Win7-<version>-bundle.zip`
- `AeroVirtIO-Win7-<version>.iso` (unless `-NoIso`; requires `cargo` for deterministic builds; legacy Windows IMAPI2 via `-LegacyIso`)
- `AeroVirtIO-Win7-<version>-fat.vhd` (when `-MakeFatImage` or `AERO_MAKE_FAT_IMAGE=1`; requires Windows + admin; skipped unless `-FatImageStrict`)

Integrity manifests (default):

- `AeroVirtIO-Win7-<version>-x86.manifest.json`
- `AeroVirtIO-Win7-<version>-x64.manifest.json`
- `AeroVirtIO-Win7-<version>-bundle.manifest.json`
- `AeroVirtIO-Win7-<version>.manifest.json` (when ISO is produced)
- `AeroVirtIO-Win7-<version>-fat.manifest.json` (when FAT VHD is produced)

Each `*.manifest.json` includes the produced artifact's `sha256` and `size`, the `version` and
`signing_policy`, and a stable list of packaged file hashes (`files[]`) for mixed-media detection.

If `-Version` is not provided, the script derives a deterministic version string from git:

- date: HEAD commit date (formatted `yyyyMMdd`)
- semver-ish: nearest `vMAJOR.MINOR.PATCH` tag + commit distance + short SHA

Resulting artifact names look like:

`AeroVirtIO-Win7-20260110-0.1.0+12.gabcdef123456-x64.zip`

## `ci/package-guest-tools.ps1`

Builds the distributable **Aero Guest Tools** media (ISO + zip) from the signed driver
packages staged under `out/packages/` (the output of `ci/make-catalogs.ps1` + `ci/sign-drivers.ps1`).

You can also point `-InputRoot` at an extracted `*-bundle.zip` (or the `.zip` file itself)
produced by `ci/package-drivers.ps1`.

When staging from `out/packages/**`, the script maps nested driver paths (`<driverRel>`) to stable,
Guest Tools-facing driver directory names so the packager spec and the `guest-tools/` repo skeleton
do not drift (e.g. `drivers/aerogpu` → `aerogpu`, `windows7/virtio-blk` → `virtio-blk`, `windows7/virtio-net` → `virtio-net`).

Driver directory name collisions:

`ci/package-guest-tools.ps1` normalizes driver directory names (lowercase + some legacy aliases like
`aero-gpu` → `aerogpu`). When staging from a **packager layout** (`x86/` + `amd64/`) or a **bundle layout**
(`drivers/<driver>/(x86|x64)/...`), the script now **fails fast** if two source directories normalize to the
same destination name (to avoid silently merging/overwriting driver contents).

To resolve a collision, remove/rename one of the input directories, or pass `-DriverNameMapJson` to override
the normalized name for a specific source directory.

This script is intended to be run after `ci/package-drivers.ps1` in CI so that the release
contains both:

- standalone driver bundles (`AeroVirtIO-Win7-*.zip` / `.iso` / optional `.vhd`)
- Guest Tools media (`aero-guest-tools.iso` / `aero-guest-tools.zip` / `manifest.json` + `aero-guest-tools.manifest.json`)

By default (`-SigningPolicy test`), it injects the public signing certificate
(`out/certs/aero-test.cer`) into the staged Guest Tools tree so the packaged installer media
trusts the exact certificate used to sign the driver catalogs.

Additionally, when staging from the CI packages layout (`out/packages/<driver>/<arch>`), the script
refuses to include any driver package that does not have a corresponding `drivers/<driver>/ci-package.json`.

For WHQL/production-signed drivers, pass `-SigningPolicy production` (or `none`) to build Guest Tools media
without injecting (or requiring) any custom certificate files.

Additionally, `aero_packager` generates `guest-tools/config/devices.cmd` from a Windows device
contract JSON during packaging (default: `docs/windows-device-contract.json`). Use
`ci/package-guest-tools.ps1 -WindowsDeviceContractPath` to override the contract when packaging
Guest Tools from a different driver stack (for example, upstream virtio-win service names).

### Optional extra Guest Tools utilities (`-ExtraToolsDir`)

`ci/package-guest-tools.ps1` can optionally stage additional guest-side helper binaries/scripts
under `tools/` in the packaged ISO/zip without requiring them to be checked into `guest-tools/`:

```powershell
pwsh -File ci/package-guest-tools.ps1 -ExtraToolsDir out/guest-tools-extra
```

This copies the contents of `-ExtraToolsDir` into the staged Guest Tools tree at `guest-tools/tools/`
before calling `aero_packager`.

By default it **merges** with any existing `guest-tools/tools/` content. To replace any existing
staged `tools/` contents instead, pass:

```powershell
pwsh -File ci/package-guest-tools.ps1 -ExtraToolsDir out/guest-tools-extra -ExtraToolsDirMode replace
```

Safety notes:

- Hidden files/dirs are skipped (for stable outputs across hosts).
- Private key material extensions are refused (e.g. `*.pfx`, `*.p12`, `*.pvk`, `*.snk`, `*.key`, `*.pem`, `*.der`, `*.p8`, `*.pk8`, `*.csr`).
- `aero_packager` applies the same default exclusions as the driver tree (e.g. `*.pdb`, `*.obj`, source files),
  so those build artifacts will not be included in the packaged `tools/` directory.

### Spec selection (CI vs local)

`ci/package-guest-tools.ps1` uses `-SpecPath` to control which driver directories are required/allowed
and how strictly hardware IDs are validated.

- Local default (when `-SpecPath` is omitted): `tools/packaging/specs/win7-aero-guest-tools.json` (stricter HWID validation)
- CI/release workflows: `tools/packaging/specs/win7-signed.json` (derives expected HWIDs from `guest-tools/config/devices.cmd`; no hardcoded regex list)

To reproduce CI packaging locally (assuming you already have `out/packages/` + `out/certs/`):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/package-guest-tools.ps1 -SpecPath tools/packaging/specs/win7-signed.json
```

## `ci/make-fat-image.ps1`

Creates a **mountable FAT32 VHD** containing a prepared driver package directory:

- `aero-test.cer` (SigningPolicy=test only)
- `INSTALL.txt`
- `x86/`
- `x64/`

`aero-test.cer` is optional when `-SigningPolicy production` (or `none`).

Notes:

- Uses **DiskPart** to create + attach the VHD and to format FAT32.
- Requires **Windows** and **Administrator** privileges.
- By default, if the environment cannot create/mount the VHD, the script **skips** FAT image creation with a warning (exit code 0). Use `-Strict` to fail instead.
