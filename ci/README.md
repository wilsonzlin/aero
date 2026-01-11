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
pwsh -File ci/make-catalogs.ps1 -ToolchainJson out/toolchain.json
pwsh -File ci/sign-drivers.ps1 -ToolchainJson out/toolchain.json
pwsh -File ci/package-drivers.ps1 -MakeFatImage
pwsh -File ci/package-guest-tools.ps1
```

## `ci/build-drivers.ps1`

Builds driver projects under `drivers/` for the requested platforms/configuration using `msbuild.exe`.

### CI packaging gate (`ci-package.json`)

For CI determinism (and to avoid accidentally shipping dev/test drivers), `ci/build-drivers.ps1` only
builds drivers that are explicitly opted into CI packaging by placing `ci-package.json` at the driver
root.

See: `drivers/_template/ci-package.json` and `ci/driver-package.schema.json`.

### Legacy WDK BUILD / NMake wrapper projects

Some legacy driver projects are Visual Studio "Makefile" projects (`<Keyword>MakeFileProj</Keyword>` /
`<ConfigurationType>Makefile</ConfigurationType>`) that invoke classic WDK 7.1 `build.exe`.
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

Only drivers that include `drivers/<driver>/ci-package.json` are staged into `out/packages/`. This is the
explicit opt-in gate that keeps CI driver bundles from accidentally including dev/test drivers.

Environment variables:

- `AERO_STAMP_INFS`: `0|false|no|off` disables stamping (default is enabled).
- `AERO_INF2CAT_OS`: overrides the `/os:` list passed to `Inf2Cat.exe` (default: `7_X64,7_X86`).

WDK redistributables (WDF coinstaller):

- **Default:** CI does not copy any WDK redistributable binaries.
- To include `WdfCoInstaller*.dll`, a driver must declare `wdfCoInstaller` in `drivers/<driver>/ci-package.json` and `ci/make-catalogs.ps1` must be run with `-IncludeWdfCoInstaller` (or `-IncludeWdkRedist WdfCoInstaller`).
- The script will fail if it detects `WdfCoInstaller*.dll` checked into the repo under `drivers/` (to prevent accidental redistribution).

Other per-driver packaging manifest features:

- `infFiles`: explicitly select which INF(s) are staged for a driver (useful when a driver ships multiple INFs with overlapping HWIDs and should not be packaged as a single combined folder).
- `wow64Files`: for x64 packages that need 32-bit user-mode components, copy specific DLLs from the x86 build output into the x64 staging directory *before* stamping INFs + running Inf2Cat.

See: `docs/16-driver-packaging-and-signing.md`.

## `ci/sign-drivers.ps1`

Test-signs staged driver packages under `out/packages/` (or `-InputRoot`) using `signtool.exe`.

CI signs:

- `*.sys` (kernel-mode drivers)
- `*.dll` (user-mode components like display driver UMDs and KMDF coinstallers)
- `*.cat` (catalogs)

And verifies:

- `.sys`: `signtool verify /kp /v`
- `.dll` + `.cat`: `signtool verify /v`

## `ci/package-drivers.ps1`

Packages signed driver staging folders from `out/packages/` into release artifacts under `out/artifacts/`.

Artifacts (typical):

- `AeroVirtIO-Win7-<version>-x86.zip`
- `AeroVirtIO-Win7-<version>-x64.zip`
- `AeroVirtIO-Win7-<version>-bundle.zip`
- `AeroVirtIO-Win7-<version>.iso` (unless `-NoIso`; Windows only)
- `AeroVirtIO-Win7-<version>-fat.vhd` (when `-MakeFatImage` or `AERO_MAKE_FAT_IMAGE=1`; requires Windows + admin; skipped unless `-FatImageStrict`)

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
do not drift (e.g. `drivers/aerogpu` → `aerogpu`, `windows7/virtio/blk` → `virtio-blk`).

This script is intended to be run after `ci/package-drivers.ps1` in CI so that the release
contains both:

- standalone driver bundles (`AeroVirtIO-Win7-*.zip` / `.iso` / optional `.vhd`)
- Guest Tools media (`aero-guest-tools.iso` / `aero-guest-tools.zip`)

By default (`-SigningPolicy testsigning`), it injects the public signing certificate
(`out/certs/aero-test.cer`) into the staged Guest Tools tree so the packaged installer media
trusts the exact certificate used to sign the driver catalogs.

For WHQL/production-signed drivers, pass `-SigningPolicy none` to build Guest Tools media
without injecting (or requiring) any custom certificate files.

Additionally, it will update the staged `guest-tools/config/devices.cmd` storage service
name (`AERO_VIRTIO_BLK_SERVICE`) to match the packaged virtio-blk driver's `AddService`
name when it can infer it from the staged INF files. This keeps `guest-tools/setup.cmd`
boot-critical pre-seeding aligned with the driver set being shipped.

## `ci/make-fat-image.ps1`

Creates a **mountable FAT32 VHD** containing a prepared driver package directory:

- `aero-test.cer`
- `INSTALL.txt`
- `x86/`
- `x64/`

Notes:

- Uses **DiskPart** to create + attach the VHD and to format FAT32.
- Requires **Windows** and **Administrator** privileges.
- By default, if the environment cannot create/mount the VHD, the script **skips** FAT image creation with a warning (exit code 0). Use `-Strict` to fail instead.
