# Driver package template

This folder is not packaged by CI. It exists as a reference for the per-driver manifest
used by the Windows driver packaging pipeline (`ci/make-catalogs.ps1`).

## Manifest

`drivers/<driver-name>/ci-package.json` is optional. If present, it can declare:

- extra non-binary files to copy into the staged package (`additionalFiles`)
- whether the driver needs a WDF coinstaller (`wdfCoInstaller`)

## Manifests

- `ci-package.json` is optional.
- See `ci-package.json` (minimal) and `ci-package.wdf-example.json` (WDF coinstaller example).
