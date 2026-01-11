# Driver package template

This folder is not packaged by CI. It exists as a reference for the per-driver manifest
used by the Windows driver packaging pipeline (`ci/make-catalogs.ps1`).

## Manifest

`drivers/<driver-name>/ci-package.json` is the per-driver packaging manifest used by CI.

CI only builds/packages drivers that include this file at the driver root (explicit opt-in),
to avoid accidentally shipping dev/test drivers or conflicting INFs.

The manifest can declare:

- extra non-binary files to copy into the staged package (`additionalFiles`)
- whether the driver needs a WDF coinstaller (`wdfCoInstaller`)

## Manifests

- `ci-package.json` is required for CI build+packaging.
- See `ci-package.json` (minimal) and `ci-package.wdf-example.json` (WDF coinstaller example).
