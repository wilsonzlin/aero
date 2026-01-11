# 16 - Windows Driver Packaging, Catalog Generation, and WDK Redistributables

## Overview

For Windows 7 compatibility, some kernel-mode drivers built with **KMDF** (and some UMDF-based stacks) may require shipping a WDF coinstaller (`WdfCoInstaller*.dll`) alongside the driver package. This file is a **Microsoft WDK redistributable** and comes with its own license terms.

This repository’s CI is designed so that **no WDK redistributable binaries are included by default**. Including any Microsoft redistributables must be an explicit choice.

---

## Per-driver packaging manifest (`ci-package.json`)

Drivers intended to be built/packaged by CI must include a manifest:

`drivers/<driver>/ci-package.json`

Where `<driver>` is a path relative to `drivers/` (it may be nested, e.g. `drivers/windows7/virtio-snd/`).

Schema: `ci/driver-package.schema.json`

This file is the **explicit CI opt-in gate**: the Win7 driver pipeline only builds/stages/packages
drivers that include `ci-package.json` at the driver root. This prevents accidentally shipping
dev/test drivers (or conflicting INFs that match the same HWIDs).

Supported fields:

- `$schema` (optional): JSON Schema reference for editor tooling (example: `"../../ci/driver-package.schema.json"`). CI ignores this field. Update the relative path as needed for nested driver directories.
- `infFiles` (optional): explicit list of `.inf` files to stage (paths relative to the driver directory). If omitted, CI discovers all `.inf` files under the driver directory.
  - Use this for drivers that ship multiple INFs (feature variants, optional components) where staging all of them together is undesirable (e.g. multiple INFs with the same HWIDs).
  - If present, the list must be non-empty.
- `wow64Files` (optional): list of **file names** to copy from the driver’s **x86** build output into the **x64** staged package directory *before* INF stamping + Inf2Cat.
  - Intended for x64 driver packages that also need 32-bit user-mode components (WOW64 UMD DLLs).
  - Entries must be `.dll` file names.
  - Entries must be file names only (no path separators).
  - Requires x86 build outputs to be present (even if you are only generating/staging x64 packages).
- `additionalFiles` (optional): extra *non-binary* files to include (README/license text, install scripts, etc). Paths are relative to the driver directory (`drivers/<driver>/`) and must not escape it (no absolute paths / `..` traversal).
- `wdfCoInstaller` (optional): declare that this driver needs the WDF coinstaller and which KMDF version/DLL name.
  - If `dllName` is omitted, CI derives it from `kmdfVersion` (e.g. `1.11` → `WdfCoInstaller01011.dll`).
  - If provided, `dllName` must be a simple filename like `WdfCoInstaller01011.dll` (not a path).

Example (explicit INF selection + WOW64 payload DLL in x64 package):

```json
{
  "$schema": "../../ci/driver-package.schema.json",
  "infFiles": ["packaging/win7/mydriver.inf"],
  "wow64Files": ["mydriver_umd.dll"],
  "additionalFiles": ["README.md", "packaging/win7/install.cmd"]
}
```

For a real in-tree example (multiple INFs + WOW64 payloads), see: `drivers/aerogpu/ci-package.json`.

Example (requires WDF coinstaller; `dllName` derived from `kmdfVersion`):

```json
{
  "$schema": "../../ci/driver-package.schema.json",
  "wdfCoInstaller": {
    "kmdfVersion": "1.11"
  },
  "additionalFiles": ["README.md", "packaging/win7/install.cmd"]
}
```

Template examples are available under `drivers/_template/`:

- `ci-package.README.md` (field reference)
- `ci-package.json` (minimal template)
- `ci-package.inf-wow64-example.json`
- `ci-package.wdf-example.json`

---

## Policy: WDK redistributables are **not included by default**

### Default behavior

- `ci/make-catalogs.ps1` stages driver packages and generates catalogs **without** copying any WDK redistributables.
- The pipeline intentionally **refuses** to package if it detects `WdfCoInstaller*.dll` checked into `drivers/<driver>/` (to prevent accidentally distributing Microsoft binaries).

### Enabling WDF coinstaller inclusion (explicit opt-in)

To allow CI to copy `WdfCoInstaller*.dll` into staged packages (from the installed WDK redist directories):

1. The driver must declare `wdfCoInstaller` in `drivers/<driver>/ci-package.json`.
2. CI must run catalog generation with **explicit opt-in**:

```powershell
.\ci\make-catalogs.ps1 -IncludeWdfCoInstaller
```

The script will copy the requested `WdfCoInstaller*.dll` into `out/packages/<driver>/<arch>/` and logs the source path used.

---

## Catalog generation and signing

`ci/make-catalogs.ps1` runs `Inf2Cat` in each staged package directory to generate `.cat` files. If a coinstaller DLL is present and referenced by the driver’s INF, Inf2Cat will hash it into the generated catalog.

Signing is handled by `ci/sign-drivers.ps1` (which uses `signtool` to sign `.sys` drivers and `.cat` catalogs):

```powershell
.\ci\sign-drivers.ps1
```

---

## Licensing note (important)

WDK redistributables (including WDF coinstallers) are **Microsoft binaries** governed by Microsoft license terms. Before distributing any package that includes them:

- review the applicable Microsoft redistribution license(s),
- confirm the distribution model is compliant,
- and document the decision.

See also: `docs/13-legal-considerations.md`.
