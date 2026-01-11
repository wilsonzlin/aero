# Windows 7 virtio-input driver package (scaffold)

This directory contains **packaging + installation infrastructure** for the Aero virtio-input Windows 7 SP1 driver.

- Target OS: **Windows 7 SP1** (x86 + x64)
- Target device: **virtio-input over PCI** (QEMU/virtio standard)
- Driver model: **KMDF HID minidriver** (`Class=HIDClass`, **KMDF 1.9**)

> This is **infrastructure only**. The actual KMDF/virtio/HID driver code (`aero_virtio_input.sys`) is intentionally not present yet.

> Note: The INF is checked in as `inf/virtio-input.inf.disabled` so CI/packaging workflows don't accidentally pick it up
> (it would conflict with the actively-built `drivers/windows/virtio-input/virtio-input.inf`). When the driver exists,
> rename it back to `inf/virtio-input.inf` before generating catalogs/signing or installing.

## KMDF version / WDF runtime (Win7 SP1)

The Windows 7 installation story is intentionally simple: the driver is built against **KMDF 1.9**, which is
**in-box** on Windows 7 SP1.

- **Built against:** KMDF **1.9**
- **Runtime on a clean Win7 SP1 machine:** present (`%SystemRoot%\System32\drivers\Wdf01000.sys`)
- **KMDF coinstaller required on Win7 SP1:** **No**
- **INF policy:** `inf/virtio-input.inf.disabled` (rename to `virtio-input.inf` when using it) pins `KmdfLibraryVersion = 1.9` and intentionally does **not** include any
  `CoInstallers32` / `WdfCoInstaller*` sections.

If you intentionally rebuild the driver against **KMDF > 1.9** (for example, by using WDK 10 defaults), Windows 7 will
require a matching WDF coinstaller/runtime in the driver package.

- The coinstaller DLL comes from the WDK you built against (typically under a `Redist\wdf\...` directory).
- WDF coinstallers/runtimes are redistributable only under the Windows Kit redistribution license. Ship unmodified files
  and consult the kit's redist/EULA documentation for exact terms.
- If you add a coinstaller:
  1. Add the matching `WdfCoInstaller010xx.dll` to `inf/`
  2. Update `virtio-input.inf` to reference it
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
| `inf/` | Driver package staging directory (INF/CAT/SYS live together for “Have Disk…” installs). |
| `scripts/` | Utilities for generating a test cert, generating the catalog, and signing. |
| `cert/` | **Local-only** output directory for `.cer/.pfx` (ignored by git). |
| `docs/` | Driver-specific notes and references. |
| `tools/` | User-mode test/diagnostic tools (currently includes `hidtest`). |
| `tests/` | Manual test plans (QEMU) and offline-install/injection notes. |

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

The INF matches both common PCI device ID schemes for virtio-input:

- **Legacy/transitional** virtio-pci ID: `PCI\VEN_1AF4&DEV_1011`
  - `0x1000 + (18 - 1) = 0x1011`
- **Modern** virtio-pci ID: `PCI\VEN_1AF4&DEV_1052`
  - `0x1040 + 18 = 0x1052`

See also: `docs/pci-hwids.md` (QEMU behavior + spec mapping).

If your emulator/QEMU build uses a different PCI device ID, update:

- `drivers/windows7/virtio-input/inf/virtio-input.inf.disabled` → `[Aero.NTx86]` / `[Aero.NTamd64]`

To confirm the IDs on Windows 7:

1. Device Manager → the device → **Properties**
2. **Details** tab → **Hardware Ids**

## Build (placeholder)

This packaging directory does not include the driver sources. Once the driver build exists, it must produce:

- `aero_virtio_input.sys`

Copy it into the driver package staging folder before generating the catalog:

```text
drivers/windows7/virtio-input/inf/aero_virtio_input.sys
```

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
inf\virtio-input.cat
```

## Signing (SYS + CAT)

From `drivers/windows7/virtio-input/`:

```cmd
.\scripts\sign-driver.cmd
```

`sign-driver.cmd` will prompt for the PFX password. You can also pass it as the first argument or set `PFX_PASSWORD` in the environment.

This signs:

- `inf\aero_virtio_input.sys`
- `inf\virtio-input.cat`

## Installation

### Device Manager (“Have Disk…”)

1. Device Manager → right-click the virtio-input PCI device → **Update Driver Software**
2. **Browse my computer**
3. **Let me pick** → **Have Disk…**
4. Browse to `drivers/windows7/virtio-input/inf/`
5. If you have not already, rename `virtio-input.inf.disabled` to `virtio-input.inf`
6. Select `virtio-input.inf`

### pnputil (Windows 7)

Windows 7 includes `pnputil.exe` but with an older CLI.

From an elevated command prompt:

```cmd
REM First, ensure the INF is named virtio-input.inf (rename from .inf.disabled if needed).
pnputil -i -a C:\path\to\virtio-input.inf
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

- This is packaging only; the driver binary is not implemented yet.
- The INF assumes the driver will be a **KMDF HID minidriver** and installs under `HIDClass`.
- The hardware ID list may need adjustment if the emulator uses a different virtio PCI ID variant.
