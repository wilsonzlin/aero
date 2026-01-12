# virtio-input (Windows 7 SP1) driver + package

This directory contains the clean-room **Aero virtio-input** Windows 7 SP1 driver (KMDF HID minidriver) and the
packaging/signing helpers needed to produce an installable driver package.

Canonical naming (see [`docs/adr/0016-win7-virtio-driver-naming.md`](../../../docs/adr/0016-win7-virtio-driver-naming.md)):

- SYS: `aero_virtio_input.sys`
- Service: `aero_virtio_input`
- INF: `inf/aero_virtio_input.inf`
- CAT: `inf/aero_virtio_input.cat`

> Note: `inf/virtio-input.inf.disabled` is a legacy filename alias kept for compatibility with older tooling/workflows
> that still reference `virtio-input.inf`. It installs the same driver/service as `inf/aero_virtio_input.inf`, but is
> disabled by default to avoid accidentally installing **two** INFs that match the same HWIDs.

## KMDF version / WDF runtime (Win7 SP1)

The Windows 7 installation story is intentionally simple: the driver is built against **KMDF 1.9**, which is
**in-box** on Windows 7 SP1.

- **Built against:** KMDF **1.9**
- **Runtime on a clean Win7 SP1 machine:** present (`%SystemRoot%\System32\drivers\Wdf01000.sys`)
- **KMDF coinstaller required on Win7 SP1:** **No**
- **INF policy:** `inf/aero_virtio_input.inf` pins `KmdfLibraryVersion = 1.9` and intentionally does **not** include any
  `CoInstallers32` / `WdfCoInstaller*` sections.

If you intentionally rebuild the driver against **KMDF > 1.9** (for example, by using WDK 10 defaults), Windows 7 will
require a matching WDF coinstaller/runtime in the driver package.

- The coinstaller DLL comes from the WDK you built against (typically under a `Redist\wdf\...` directory).
- WDF coinstallers/runtimes are redistributable only under the Windows Kit redistribution license. Ship unmodified files
  and consult the kit's redist/EULA documentation for exact terms.
- If you add a coinstaller:
  1. Add the matching `WdfCoInstaller010xx.dll` to `inf/`
  2. Update `aero_virtio_input.inf` to reference it
  3. Regenerate the catalog and re-sign
  4. Ensure release packaging includes it (see `release/README.md`)

## Toolchain selection

### Default (recommended): WDK 7.1 + KMDF 1.9 (no coinstaller)

- **WDK:** Windows Driver Kit **7.1** (7600.16385.1)
- **Compiler/IDE:** Visual Studio **2010**/**2012** (or the WDK 7.1 build environment)

This is the default because it naturally targets **KMDF 1.9**, which is **in-box** on Windows 7 SP1.
That keeps the installable driver package minimal (`.inf` + `.sys` + optional `.cat`).

### Alternative: WDK 10 / VS2019+ (requires shipping WDF for Win7 if KMDF > 1.9)

WDK 10 is fine for running tools like `Inf2Cat.exe` / `signtool.exe`, but if the driver binary is built against a newer
KMDF than 1.9, you must ship and install the matching WDF coinstaller/runtime on Windows 7.

## Directory layout

| Path | Purpose |
| --- | --- |
| `src/` | Driver source code (KMDF HID minidriver). |
| `inf/` | Driver package staging directory (INF/CAT/SYS live together for “Have Disk…” installs). |
| `scripts/` | Utilities for generating a test cert, generating the catalog, and signing. |
| `cert/` | **Local-only** output directory for `.cer/.pfx` (ignored by git). |
| `docs/` | Driver-specific notes and references. |
| `tools/` | User-mode test/diagnostic tools (currently includes `hidtest`). |
| `tests/` | Unit tests, manual test plans (QEMU), and offline-install/injection notes. |

## Prerequisites (host build/sign machine)

Any Windows machine that can run the WDK tools.

You need the following tools in `PATH` (usually by opening a WDK Developer Command Prompt):

- `Inf2Cat.exe`
- `signtool.exe`
- `certutil.exe` (built into Windows)

## Prerequisites (Windows 7 test VM / machine)

1. Enable test-signing mode (elevated cmd):

   ```cmd
   bcdedit /set testsigning on
   shutdown /r /t 0
   ```

2. Install the generated test certificate into the machine trust stores (see below).

## Hardware IDs (PnP IDs)

The canonical INF (`inf/aero_virtio_input.inf`) intentionally matches only **Aero contract v1** hardware IDs:

- `PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01` (keyboard)
- `PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01` (mouse)
- `PCI\VEN_1AF4&DEV_1052&REV_01` (fallback for environments that do not expose the Aero subsystem IDs)

It does **not** match:

- the transitional virtio-input PCI ID (`DEV_1011`), or
- any non-revision-gated variants (no `&REV_01`).

See also: `docs/pci-hwids.md` (QEMU behavior + spec mapping).

If your emulator/QEMU build uses a different PCI device ID, update:

- `drivers/windows7/virtio-input/inf/aero_virtio_input.inf` → `[Aero.NTx86]` / `[Aero.NTamd64]`

To confirm the IDs on Windows 7:

1. Device Manager → the device → **Properties**
2. **Details** tab → **Hardware Ids**

## Build

### Supported: WDK10 / MSBuild (CI path)

CI builds this driver via the MSBuild project:

- `drivers/windows7/virtio-input/aero_virtio_input.vcxproj`

From a Windows host with the WDK installed:

```powershell
# From the repo root:
.\ci\install-wdk.ps1
.\ci\build-drivers.ps1 -ToolchainJson .\out\toolchain.json -Drivers windows7/virtio-input
```

Build outputs are staged under:

- `out/drivers/windows7/virtio-input/x86/aero_virtio_input.sys`
- `out/drivers/windows7/virtio-input/x64/aero_virtio_input.sys`

The MSBuild project pins `KmdfLibraryVersion = 1.9` so the built driver should load on a stock Windows 7 SP1 machine without a coinstaller (matching the INF policy described above).

### Legacy/deprecated: WDK 7.1 `build.exe`

This driver can also be built with the classic WDK 7.1 `build` utility (so KMDF 1.9 is targeted naturally).

1. Open the WDK build environment:
   - `Windows 7 x86 Free Build Environment`
   - `Windows 7 x64 Free Build Environment`
2. Build from the driver root:

```bat
cd \path\to\repo\drivers\windows7\virtio-input
build -cZ
```

The built `aero_virtio_input.sys` is placed under the WDK output directory (e.g. `objfre_win7_x86\i386\` or
`objfre_win7_amd64\amd64\`).

To generate a catalog locally, copy the built SYS into the package staging folder:

```text
drivers/windows7/virtio-input/inf/aero_virtio_input.sys
```

Instead of copying manually, you can use:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\stage-built-sys.ps1 -Arch amd64
```

If you built via the CI/MSBuild pipeline (which places outputs under `out/drivers/...`), run from the repo root:

```powershell
powershell -ExecutionPolicy Bypass -File drivers/windows7/virtio-input/scripts/stage-built-sys.ps1 `
  -Arch amd64 `
  -InputDir out/drivers/windows7/virtio-input
```

To produce a signed, redistributable ZIP in one step (stages SYS → Inf2Cat → sign → package), run:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-release.ps1 -Arch both -InputDir <build-output-root>
```

Output is written to `release/out/`.

## Test certificate workflow (generate + install)

### 1) Generate a test certificate (on the signing machine)

From `drivers/windows7/virtio-input/`:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\make-cert.ps1
```

`make-cert.ps1` defaults to generating a **SHA-1-signed** test certificate for maximum compatibility with stock Windows 7 SP1.
If your environment cannot create SHA-1 certificates, you can opt into SHA-2 by rerunning with:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\make-cert.ps1 -AllowSha2CertFallback
```

> A SHA-2-signed certificate may require Windows 7 SHA-2 updates (KB3033929 / KB4474419) on the test machine.

Expected outputs:

```text
cert\aero-virtio-input-test.cer
cert\aero-virtio-input-test.pfx
```

> Do **not** commit `.pfx` files. Treat them like private keys.

### 2) Install the test certificate (on the Windows 7 test machine)

Copy `cert\aero-virtio-input-test.cer` to the test machine, then run (elevated PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-test-cert.ps1 -CertPath .\cert\aero-virtio-input-test.cer
```

This installs the cert into:

- LocalMachine **Trusted Root Certification Authorities**
- LocalMachine **Trusted Publishers**

## Catalog generation (CAT)

From `drivers/windows7/virtio-input/`:

```cmd
.\scripts\make-cat.cmd
```

This runs `Inf2Cat` for both architectures:

- `7_X86`
- `7_X64`

Expected output (once `aero_virtio_input.sys` exists in `inf/`):

```text
inf\aero_virtio_input.cat
```

## Signing (SYS + CAT)

From `drivers/windows7/virtio-input/`:

```cmd
.\scripts\sign-driver.cmd
```

`sign-driver.cmd` will prompt for the PFX password. You can also pass it as the first argument or set `PFX_PASSWORD` in the environment.

This signs:

- `inf\aero_virtio_input.sys`
- `inf\aero_virtio_input.cat`

## Installation

### Device Manager (“Have Disk…”)

1. Device Manager → right-click the virtio-input PCI device → **Update Driver Software**
2. **Browse my computer**
3. **Let me pick** → **Have Disk…**
4. Browse to `drivers/windows7/virtio-input/inf/`
5. Select `aero_virtio_input.inf`

### pnputil (Windows 7)

Windows 7 includes `pnputil.exe` but with an older CLI.

From an elevated command prompt:

```cmd
pnputil -i -a C:\path\to\aero_virtio_input.inf
```

## Verifying the driver loaded

### Device Manager

- The device should move under **Human Interface Devices** (HIDClass).
- Driver details should show `aero_virtio_input.sys`.

### Service state

```cmd
sc query aero_virtio_input
```

### Driver file present

```cmd
dir %SystemRoot%\System32\drivers\aero_virtio_input.sys
```

## QEMU / emulator notes (expected device)

virtio-input appears as a **PCI virtio** function. In QEMU this is typically created with devices like:

- `virtio-keyboard-pci`
- `virtio-mouse-pci`
- `virtio-tablet-pci`

All of these use the virtio-input transport and should enumerate with `VEN_1AF4` and a virtio-input device ID (commonly `DEV_1011` for legacy/transitional or `DEV_1052` for modern).

## Testing

- User-mode HID verification tool: `tools/hidtest/README.md`
- Manual QEMU test plan: `tests/qemu/README.md`
- Offline/slipstream install notes (DISM): `tests/offline-install/README.md`

## Release packaging (optional)

Once the driver binary exists, you can produce a deterministic, redistributable ZIP bundle using:

- `release/README.md`
- `scripts/package-release.ps1`

## Known limitations

- The driver is still under active development; it does not yet provide complete virtio-input functionality.
- The INF installs the driver as a **KMDF HID minidriver** under `HIDClass`.
- The hardware ID list may need adjustment if the emulator uses a different virtio PCI ID variant.
