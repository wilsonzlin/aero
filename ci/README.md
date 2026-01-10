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

Example local usage:

```powershell
pwsh -File ci/install-wdk.ps1
pwsh -File ci/build-drivers.ps1 -ToolchainJson out/toolchain.json
pwsh -File ci/make-catalogs.ps1 -ToolchainJson out/toolchain.json
pwsh -File ci/sign-drivers.ps1 -ToolchainJson out/toolchain.json
pwsh -File ci/package-drivers.ps1
```

## `ci/stamp-infs.ps1`

Stamps `DriverVer` in staged `.inf` files (in-place) using WDK `stampinf.exe`.

Defaults (when overrides are not provided):

- **Date**: the HEAD commit date (`git show -s --format=%cI HEAD`), clamped to build time so it is never in the future.
- **Version**: derived from the nearest `vMAJOR.MINOR.PATCH` git tag + commit distance:
  - `DriverVer` version: `MAJOR.MINOR.PATCH.<distance>`
  - Package version (for logs/artifact naming): `MAJOR.MINOR.PATCH+<distance>.g<shortsha>`

This script only stamps INFs inside the provided staging directory.

## `ci/make-catalogs.ps1`

Runs `ci/stamp-infs.ps1` **before** calling `Inf2Cat.exe`, because catalog hashes include the INF contents.

Environment variables:

- `AERO_STAMP_INFS`: `0|false|no|off` disables stamping (default is enabled).
- `AERO_INF2CAT_OS`: overrides the `/os:` list passed to `Inf2Cat.exe` (default: `7_X64,7_X86`).

## `ci/package-drivers.ps1`

Packages signed driver staging folders from `out/packages/` into release artifacts under `out/artifacts/`.

If `-Version` is not provided, the script derives a deterministic version string from git:

- date: HEAD commit date (formatted `yyyyMMdd`)
- semver-ish: nearest `vMAJOR.MINOR.PATCH` tag + commit distance + short SHA

Resulting artifact names look like:

`AeroVirtIO-Win7-20260110-0.1.0+12.gabcdef123456-x64.zip`
