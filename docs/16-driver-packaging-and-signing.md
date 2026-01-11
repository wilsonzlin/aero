# 16 - Windows Driver Packaging, Catalog Generation, and WDK Redistributables

## Overview

For Windows 7 compatibility, some kernel-mode drivers built with **KMDF** (and some UMDF-based stacks) may require shipping a WDF coinstaller (`WdfCoInstaller*.dll`) alongside the driver package. This file is a **Microsoft WDK redistributable** and comes with its own license terms.

This repository’s CI is designed so that **no WDK redistributable binaries are included by default**. Including any Microsoft redistributables must be an explicit choice.

---

## Per-driver packaging manifest (`ci-package.json`)

Each driver may include an optional manifest:

`drivers/<name>/ci-package.json`

Schema: `ci/driver-package.schema.json`

Supported fields:

- `infFiles` (optional): explicit list of `.inf` files to stage (paths relative to the driver directory). If omitted, CI discovers all `.inf` files under the driver directory.
  - Use this for drivers that ship multiple INFs (feature variants, optional components) where staging all of them together is undesirable (e.g. multiple INFs with the same HWIDs).
- `wow64Files` (optional): list of **file names** to copy from the driver’s **x86** build output into the **x64** staged package directory *before* INF stamping + Inf2Cat.
  - Intended for x64 driver packages that also need 32-bit user-mode components (WOW64 UMD DLLs).
- `additionalFiles` (optional): extra *non-binary* files to include (README/license text, install scripts, etc). Paths are relative to the driver directory (`drivers/<name>/`) and must not escape it (no absolute paths / `..` traversal).
- `wdfCoInstaller` (optional): declare that this driver needs the WDF coinstaller and which KMDF version/DLL name.
  - If `dllName` is omitted, CI derives it from `kmdfVersion` (e.g. `1.11` → `WdfCoInstaller01011.dll`).
  - If provided, `dllName` must be a simple filename like `WdfCoInstaller01011.dll` (not a path).

Example (requires WDF coinstaller):

```json
{
  "$schema": "../../ci/driver-package.schema.json",
  "wdfCoInstaller": {
    "kmdfVersion": "1.11",
    "dllName": "WdfCoInstaller01011.dll"
  },
  "additionalFiles": ["README.md", "packaging/win7/install.cmd"]
}
```

---

## Policy: WDK redistributables are **not included by default**

### Default behavior

- `ci/make-catalogs.ps1` stages driver packages and generates catalogs **without** copying any WDK redistributables.
- The pipeline intentionally **refuses** to package if it detects `WdfCoInstaller*.dll` checked into `drivers/<name>/` (to prevent accidentally distributing Microsoft binaries).

### Enabling WDF coinstaller inclusion (explicit opt-in)

To allow CI to copy `WdfCoInstaller*.dll` into staged packages (from the installed WDK redist directories):

1. The driver must declare `wdfCoInstaller` in `drivers/<name>/ci-package.json`.
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
