# Driver package stamping & catalog generation

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
