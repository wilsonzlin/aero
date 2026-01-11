# CI driver packaging manifest (`ci-package.json`)

Drivers intended to be built/packaged by CI must include a manifest at:

`drivers/<driver>/ci-package.json`

This file is consumed by `ci/make-catalogs.ps1` to control what gets staged into
`out/packages/<driver>/<arch>/` before INF stamping + `Inf2Cat`.

For the canonical documentation, see: [`docs/16-driver-packaging-and-signing.md`](../../docs/16-driver-packaging-and-signing.md).

## Fields

### `$schema` (optional)

JSON Schema reference for editor tooling (example: `"../../ci/driver-package.schema.json"`). CI ignores this field.
Update the relative path as needed if your driver directory is nested (for example, `drivers/windows7/<driver>/`).

### `infFiles` (optional)

Explicit list of `.inf` files to stage (paths are **relative to the driver directory**).

- If omitted, CI discovers all `*.inf` files under the driver directory.
- Use this when a driver ships **multiple INFs** but only a subset should be packaged together
  (feature variants, optional components, etc).
- If present, the list must be non-empty.

Example:

```json
{ "infFiles": ["packaging/win7/mydriver.inf"] }
```

### `wow64Files` (optional)

List of **file names** (no path separators) of 32-bit DLLs to copy from the driver's **x86**
build output into the **x64** staged package directory.

This is required for x64 packages that ship WOW64 user-mode components (for example, a 32-bit
display UMD DLL installed into `SysWOW64`). `Inf2Cat` needs the WOW64 payload to be present in
the x64 staging directory at catalog-generation time.

- Entries must have a `.dll` extension.
- Requires x86 build outputs to be present even if you are only generating/staging x64 packages.

Example:

```json
{ "wow64Files": ["mydriver_umd.dll"] }
```

### `additionalFiles` (optional)

Extra files (paths relative to the driver directory) to include in staged packages. Intended
for **non-binary** assets like README/license text or helper scripts.

### `wdfCoInstaller` (optional, opt-in)

Declare that the driver package requires a WDF coinstaller (`WdfCoInstaller*.dll`).

- This is a Microsoft WDK redistributable and is **not shipped by default**.
- Do **not** commit `WdfCoInstaller*.dll` into the repo; CI will refuse to package if it finds
  one under `drivers/<driver>/`.
- To include it, you must:
  1. declare `wdfCoInstaller` in the manifest, and
  2. run `ci/make-catalogs.ps1` with `-IncludeWdfCoInstaller`.

Examples:

- `ci-package.json` (minimal)
- `ci-package.inf-wow64-example.json` (INF selection + WOW64 payload DLL example)
- `ci-package.wdf-example.json` (WDF coinstaller example)
