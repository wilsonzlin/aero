<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Windows 7 virtio-snd driver package (build + signing)

This directory contains the Aero **virtio-snd** Windows 7 SP1 driver sources, plus scripts to produce an **installable, test-signed** driver package.

The intended developer workflow is:

1. Build `aero_virtio_snd.sys`
2. Copy it into `drivers/windows7/virtio-snd/inf/`
3. Generate a test certificate, generate a catalog (`.cat`), and sign `SYS + CAT`
4. Install on Windows 7 with test-signing enabled (Device Manager → “Have Disk…”)

## Directory layout

| Path | Purpose |
| --- | --- |
| `SOURCES.md` | Clean-room/source tracking record (see `drivers/windows7/LEGAL.md` §2.6). |
| `src/`, `include/` | Driver sources (shared by both build systems). |
| `aero_virtio_snd.vcxproj` | **CI-supported** MSBuild project (WDK10; builds `aero_virtio_snd.sys`). |
| `makefile`, `src/sources` | Legacy WinDDK 7600 / WDK 7.1 `build.exe` files (deprecated). |
| `inf/` | Driver package staging directory (INF/CAT/SYS live together for “Have Disk…” installs). |
| `scripts/` | Utilities for generating a test cert, generating the catalog, signing, and optional release packaging. |
| `cert/` | **Local-only** output directory for `.cer/.pfx` (ignored by git). |
| `release/` | Release packaging docs and output directory (ignored by git). |
| `docs/` | Driver implementation notes / references. |

## Prerequisites (host build/sign machine)

Any Windows machine that can run the Windows Driver Kit tooling.

You need the following tools in `PATH` (typically by opening a WDK Developer Command Prompt):

- `Inf2Cat.exe`
- `signtool.exe`
- `certutil.exe` (built into Windows)

## Build

### Supported: WDK10 / MSBuild (CI path)

This driver is built in CI via the MSBuild project:

- `drivers/windows7/virtio-snd/aero_virtio_snd.vcxproj`

From a Windows host with the WDK installed:

```powershell
# From the repo root:
.\ci\install-wdk.ps1
.\ci\build-drivers.ps1 -ToolchainJson .\out\toolchain.json -Drivers windows7/virtio-snd
```

Build outputs are staged under:

- `out/drivers/windows7/virtio-snd/x86/aero_virtio_snd.sys`
- `out/drivers/windows7/virtio-snd/x64/aero_virtio_snd.sys`

Optional (QEMU/transitional) build outputs:

- `out/drivers/windows7/virtio-snd/x86/virtiosnd_legacy.sys`
- `out/drivers/windows7/virtio-snd/x64/virtiosnd_legacy.sys`

Optional (legacy virtio-pci **I/O-port** bring-up) build:

- MSBuild project: `drivers/windows7/virtio-snd/virtio-snd-ioport-legacy.vcxproj`
- Output SYS: `virtiosnd_ioport.sys`
- INF: `drivers/windows7/virtio-snd/inf/aero-virtio-snd-ioport.inf`

To stage an installable/signable package, copy the appropriate `aero_virtio_snd.sys` into:

```text
drivers/windows7/virtio-snd/inf/aero_virtio_snd.sys
```

For the optional QEMU/transitional package, stage the legacy binary instead:

```text
drivers/windows7/virtio-snd/inf/virtiosnd_legacy.sys
```

### Legacy/deprecated: WinDDK 7600 `build.exe`

The original WinDDK 7600 `build.exe` files are kept for reference. See `docs/README.md` for legacy build environment notes.

The build must produce:

- `aero_virtio_snd.sys`

Copy the built driver into the package staging folder:

```text
drivers/windows7/virtio-snd/inf/aero_virtio_snd.sys
```

Instead of copying manually, you can use:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\stage-built-sys.ps1 -Arch amd64
```

For the optional transitional/QEMU package:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\stage-built-sys.ps1 -Arch amd64 -Variant legacy
```

To build a signed `release/` package in one step (stages SYS → Inf2Cat → sign → package):

```powershell
# Contract v1 (default):
powershell -ExecutionPolicy Bypass -File .\scripts\build-release.ps1 -Arch both -InputDir <build-output-root>

# Transitional/QEMU:
powershell -ExecutionPolicy Bypass -File .\scripts\build-release.ps1 -Arch both -Variant legacy -InputDir <build-output-root>
```

Add `-Zip` to also create deterministic `release/out/*.zip` bundles.

## Windows 7 test-signing enablement (test VM / machine)

On the Windows 7 test machine, enable test-signing mode from an elevated cmd prompt:

```cmd
bcdedit /set testsigning on
shutdown /r /t 0
```

## Test certificate workflow (generate + install)

### 1) Generate a test certificate (on the signing machine)

From `drivers/windows7/virtio-snd/`:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\make-cert.ps1
```

`make-cert.ps1` defaults to generating a **SHA-1-signed** test certificate for maximum compatibility with stock Windows 7 SP1.
If your environment cannot create SHA-1 certificates, you can opt into SHA-2 by rerunning with:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\make-cert.ps1 -AllowSha2CertFallback
```

Expected outputs:

```text
cert\aero-virtio-snd-test.cer
cert\aero-virtio-snd-test.pfx
```

> Do **not** commit `.pfx` files. Treat them like private keys.

### 2) Install the test certificate (on the Windows 7 test machine)

Copy `cert\aero-virtio-snd-test.cer` to the test machine, then run from an **elevated** PowerShell prompt:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-test-cert.ps1 -CertPath .\cert\aero-virtio-snd-test.cer
```

This installs the cert into:

- LocalMachine **Trusted Root Certification Authorities**
- LocalMachine **Trusted Publishers**

## Catalog generation (CAT)

From `drivers/windows7/virtio-snd/`:

```cmd
.\scripts\make-cat.cmd
```

This runs `Inf2Cat` for both architectures:

- `7_X86`
- `7_X64`

Expected output (once `aero_virtio_snd.sys` exists in `inf/`):

```text
inf\aero_virtio_snd.cat
```

To generate the optional QEMU/transitional catalog (for `aero-virtio-snd-legacy.inf` / `virtiosnd_legacy.sys`), run:

```cmd
.\scripts\make-cat.cmd legacy
```
## Signing (SYS + CAT)

From `drivers/windows7/virtio-snd/`:

```cmd
.\scripts\sign-driver.cmd [contract|legacy|all] [PFX_PASSWORD]
```

`sign-driver.cmd` will prompt for the PFX password. You can also set `PFX_PASSWORD` in the environment.

Notes:

- The default variant is `contract`.
- Backwards compatible: if the first argument is not a variant, it is treated as the PFX password (and the `contract` variant is used).

This signs (contract v1):

- `inf\aero_virtio_snd.sys`
- `inf\aero_virtio_snd.cat`
- `inf\virtiosnd_legacy.sys` (if present)
- `inf\aero-virtio-snd-legacy.cat` (if present)

To sign the optional transitional/QEMU package, run:

```cmd
.\scripts\sign-driver.cmd legacy
```

This signs:

- `inf\virtiosnd_legacy.sys`
- `inf\aero-virtio-snd-legacy.cat`

## Installation (Device Manager → “Have Disk…”)

1. Device Manager → right-click the virtio-snd PCI device → **Update Driver Software**
2. **Browse my computer**
3. **Let me pick** → **Have Disk…**
4. Browse to `drivers/windows7/virtio-snd/inf/`
5. Select `aero_virtio_snd.inf` (recommended for Aero contract v1)
   - For stock QEMU defaults (transitional virtio-snd PCI IDs; typically `PCI\VEN_1AF4&DEV_1018`), select `aero-virtio-snd-legacy.inf`

`virtio-snd.inf.disabled` is a legacy filename alias kept for compatibility with older workflows/tools that still reference
`virtio-snd.inf`. It installs the same driver/service and matches the same contract-v1 HWIDs as `aero_virtio_snd.inf`, but
is disabled by default to avoid accidentally installing **two** INFs that match the same HWIDs.

## Offline / slipstream installation (optional)

If you want virtio-snd to bind automatically on first boot (for example when building unattended Win7 images), see:

- `tests/offline-install/README.md`

## Manual QEMU test plan
  
For a repeatable manual bring-up/validation plan under QEMU, see:

- `tests/qemu/README.md`

## Host unit tests (Linux/macOS)

Kernel drivers cannot run in CI, but parts of the virtio-snd protocol engines can
be compiled and unit tested on the host (descriptor/SG building, framing, and
status/state handling).

From the repo root:

```sh
cmake -S drivers/windows7/virtio-snd/tests/host -B out/virtiosnd-host-tests
cmake --build out/virtiosnd-host-tests
ctest --test-dir out/virtiosnd-host-tests
```

## Release packaging (optional)

Once the package has been built/signed, you can stage a Guest Tools–ready folder under `release\<arch>\virtio-snd\` using:

- `scripts/package-release.ps1` (see `release/README.md`)

The same script can also produce a deterministic ZIP bundle from `inf/` by passing `-Zip`.
