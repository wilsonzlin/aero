# Driver package template

This folder is not packaged by CI. It exists as a reference for the per-driver manifest
used by the Windows driver packaging pipeline (`ci/make-catalogs.ps1`).

## Manifest

`drivers/<driver-name>/ci-package.json` is the per-driver packaging manifest used by CI.

CI only builds/packages drivers that include this file at the driver root (explicit opt-in),
to avoid accidentally shipping dev/test drivers or conflicting INFs.

The manifest can declare:

- explicit list of `.inf` files to stage (`infFiles`) to avoid packaging multiple optional INFs together
  - paths are relative to the driver directory
  - if present, the list must be non-empty
- WOW64 payload DLL file names to copy from x86 build outputs into the x64 staged package (`wow64Files`)
  - entries must be file names only (no path separators)
- extra non-binary files to copy into the staged package (`additionalFiles`)
- whether the driver needs a WDF coinstaller (`wdfCoInstaller`)

## Files

- `ci-package.json`: minimal template (this file is required for CI build+packaging in real driver directories). Add `infFiles` and/or update `wow64Files` as needed.
- `ci-package.inf-wow64-example.json`: example manifest showing `infFiles` + `wow64Files` usage (explicit INF selection + WOW64 payload DLLs).
- `ci-package.wdf-example.json`: WDF coinstaller example.
