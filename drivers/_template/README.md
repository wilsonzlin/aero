# Driver package template

This folder is not packaged by CI. It exists as a reference for the per-driver manifest
used by the Windows driver packaging pipeline (`ci/make-catalogs.ps1`).

## Manifest

`drivers/<driver>/ci-package.json` is the per-driver packaging manifest used by CI.

CI only builds/packages drivers that include this file at the driver root (explicit opt-in),
to avoid accidentally shipping dev/test drivers or conflicting INFs.

The manifest can declare:

- `$schema` (optional): JSON Schema reference for editor tooling. CI ignores this field, but it can be useful for validation/autocomplete in editors. Update the relative path if your driver lives deeper than `drivers/<driver>/`.
- explicit list of `.inf` files to stage (`infFiles`) to avoid packaging multiple optional INFs together
  - paths are relative to the driver directory
  - if omitted, CI stages all `.inf` files discovered under the driver directory
  - if present, the list must be non-empty
- WOW64 payload DLL file names to copy from x86 build outputs into the x64 staged package (`wow64Files`)
  - entries must be file names only (no path separators)
- extra non-binary files to copy into the staged package (`additionalFiles`)
- whether the driver needs a WDF coinstaller (`wdfCoInstaller`, explicit opt-in)

See `ci-package.README.md` in this directory for a short field reference, and the canonical
doc at `docs/16-driver-packaging-and-signing.md` for details.

## Files

- `ci-package.json`: minimal template (this file is required for CI build+packaging in real driver directories). Add `infFiles` and/or update `wow64Files` as needed.
- `ci-package.README.md`: field-by-field reference (canonical documentation lives under `docs/16-driver-packaging-and-signing.md`).
- `ci-package.inf-wow64-example.json`: example manifest showing `infFiles` + `wow64Files` usage (explicit INF selection + WOW64 payload DLLs).
- `ci-package.wdf-example.json`: WDF coinstaller example.
