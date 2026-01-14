# virtio-input (Windows 7 SP1) driver + package

This directory contains the clean-room **Aero virtio-input** Windows 7 SP1 driver (KMDF HID minidriver) and the
packaging/signing helpers needed to produce an installable driver package.

End-to-end validation plan (device model + driver + web runtime):

- [`docs/virtio-input-test-plan.md`](../../../docs/virtio-input-test-plan.md)

Canonical naming (see [`docs/adr/0016-win7-virtio-driver-naming.md`](../../../docs/adr/0016-win7-virtio-driver-naming.md)):

- SYS: `aero_virtio_input.sys`
- Service: `aero_virtio_input`
- INFs:
  - `inf/aero_virtio_input.inf` (keyboard + mouse)
  - `inf/aero_virtio_tablet.inf` (tablet / absolute pointer)
- CATs:
  - `inf/aero_virtio_input.cat`
  - `inf/aero_virtio_tablet.cat`

> Note: `inf/virtio-input.inf.disabled` is a **legacy filename alias** for compatibility with older tooling/workflows
> that still reference `virtio-input.inf` instead of `aero_virtio_input.inf`.
>
> The alias INF is checked in disabled-by-default; rename it to `virtio-input.inf` to enable it locally if needed.
>
> Policy:
>
> - The canonical keyboard/mouse INF (`inf/aero_virtio_input.inf`) is intentionally **SUBSYS-only**:
>   - It matches only the subsystem-qualified keyboard/mouse HWIDs (`SUBSYS_0010` / `SUBSYS_0011`) for distinct Device Manager names.
>   - It does **not** include the strict generic (no `SUBSYS`) fallback model line (`PCI\VEN_1AF4&DEV_1052&REV_01`).
> - The legacy alias INF is allowed to diverge from `inf/aero_virtio_input.inf` **only** in the models sections
>   (`[Aero.NTx86]` / `[Aero.NTamd64]`) where it adds the opt-in strict revision-gated generic fallback HWID (no `SUBSYS`):
>   `PCI\VEN_1AF4&DEV_1052&REV_01` (Device Manager name: **Aero VirtIO Input Device**).
>   - Outside those models sections, from the first section header (`[Version]`) onward, it is expected to be byte-for-byte
>     identical to `inf/aero_virtio_input.inf` (banner/comments may differ; helper: `scripts/check-inf-alias.py`;
>     CI: `scripts/ci/check-windows7-virtio-contract-consistency.py`).
>   - Enabling the alias **does** change HWID matching behavior (it enables generic fallback binding).
>
> Do not ship/install the alias alongside `aero_virtio_input.inf`: the alias overlaps the SUBSYS keyboard/mouse HWIDs
> **and** adds the generic fallback, which can lead to confusing PnP selection. Ship/install **only one** of the two
> basenames (`aero_virtio_input.inf` *or* `virtio-input.inf`) at a time.
> (Tablet uses the separate `inf/aero_virtio_tablet.inf` and wins when its HWID matches, due to specificity.)

## KMDF version / WDF runtime (Win7 SP1)

The Windows 7 installation story is intentionally simple: the driver is built against **KMDF 1.9**, which is
**in-box** on Windows 7 SP1.

- **Built against:** KMDF **1.9**
- **Runtime on a clean Win7 SP1 machine:** present (`%SystemRoot%\System32\drivers\Wdf01000.sys`)
- **KMDF coinstaller required on Win7 SP1:** **No**
- **INF policy:** `inf/aero_virtio_{input,tablet}.inf` pin `KmdfLibraryVersion = 1.9` and intentionally do **not**
  include any `CoInstallers32` / `WdfCoInstaller*` sections.

If you intentionally rebuild the driver against **KMDF > 1.9** (for example, by using WDK 10 defaults), Windows 7 will
require a matching WDF coinstaller/runtime in the driver package.

- The coinstaller DLL comes from the WDK you built against (typically under a `Redist\wdf\...` directory).
- WDF coinstallers/runtimes are redistributable only under the Windows Kit redistribution license. Ship unmodified files
  and consult the kit's redist/EULA documentation for exact terms.
- If you add a coinstaller:
  1. Add the matching `WdfCoInstaller010xx.dll` to `inf/`
  2. Update `aero_virtio_input.inf` and `aero_virtio_tablet.inf` to reference it
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

The in-tree INFs intentionally match only **Aero contract v1** hardware IDs (revision-gated `REV_01`):

- `inf/aero_virtio_input.inf` (keyboard/mouse; canonical):
  - `PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01` (keyboard)
  - `PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01` (mouse)
  - Note: intentionally **SUBSYS-only** (no strict generic fallback).
- `inf/aero_virtio_tablet.inf` (tablet / absolute pointer):
  - `PCI\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01` (tablet / absolute pointer)
- Legacy filename alias (disabled by default): `inf/virtio-input.inf.disabled`
  - Rename it to `virtio-input.inf` to enable it (for workflows/tools that still reference `virtio-input.inf`, and/or to opt into strict generic fallback binding).
  - It adds the strict generic fallback HWID (no `SUBSYS`) in both models sections:
    - `PCI\VEN_1AF4&DEV_1052&REV_01` (**Aero VirtIO Input Device**)
  - Alias sync policy: outside the models sections (`[Aero.NTx86]` / `[Aero.NTamd64]`), from the first section header (`[Version]`) onward,
    expected to match `inf/aero_virtio_input.inf` byte-for-byte (banner/comments may differ; see `scripts/check-inf-alias.py`).
  - Enabling the alias **does** change HWID matching behavior, and because it overlaps the SUBSYS keyboard/mouse HWIDs **and**
    adds the fallback, do **not** ship/install it alongside `aero_virtio_input.inf` (ship/install only one INF basename at a time).

The subsystem-qualified IDs use distinct `DeviceDesc` strings, so when the device exposes the Aero subsystem IDs the PCI functions appear as separate named devices in
Device Manager (**Aero VirtIO Keyboard** / **Aero VirtIO Mouse** / **Aero VirtIO Tablet Device**).

When binding via the legacy alias INF's generic fallback model line (`PCI\VEN_1AF4&DEV_1052&REV_01`), Device Manager will show
the generic **Aero VirtIO Input Device** name.

The tablet INF is more specific (`SUBSYS_0012...`), so it wins over the generic fallback when both are installed and the tablet
subsystem ID is present. If the tablet INF is not installed (or the device does not expose the tablet subsystem ID), the generic
fallback entry (when enabled via the alias INF) can also bind to tablet devices (but will use the generic device name).

The INFs do **not** match:

- the transitional virtio-input PCI ID (`DEV_1011`), or
- any non-revision-gated variants (no `&REV_01`).

See also: `docs/pci-hwids.md` (QEMU behavior + spec mapping).

### Static INF validation (`verify-inf.ps1`)

To catch accidental INF edits that would break Aero’s Windows 7 virtio-input packaging/contract expectations, run:

```powershell
.\scripts\verify-inf.ps1
```

This performs a lightweight static check (string/regex based) over `inf/aero_virtio_input.inf` by default and exits non-zero with an actionable error list if anything required is missing.

To validate the optional legacy filename alias INF stays in sync with the canonical INF **outside the models sections**
(`[Aero.NTx86]` / `[Aero.NTamd64]`) from the first section header (`[Version]`) onward (banner/comments may differ), run:

```powershell
python .\scripts\check-inf-alias.py
```

If your emulator/QEMU build uses a different PCI device ID, update:

- `drivers/windows7/virtio-input/inf/aero_virtio_input.inf` → `[Aero.NTx86]` / `[Aero.NTamd64]` (keyboard/mouse)
- `drivers/windows7/virtio-input/inf/aero_virtio_tablet.inf` → `[Aero.NTx86]` / `[Aero.NTamd64]` (tablet)

To confirm the IDs on Windows 7:

1. Device Manager → the device → **Properties**
2. **Details** tab → **Hardware Ids**

## Optional/Compatibility Features

### Interrupts: INTx baseline, optional MSI/MSI-X

Per the [`AERO-W7-VIRTIO` v1 contract](../../../docs/windows7-virtio-driver-contract.md) (§1.8), **INTx is required**
and MSI/MSI-X is an optional enhancement.

MSI/MSI-X must not be required for functionality: if Windows does not allocate MSI/MSI-X, the driver is expected to
use INTx.

#### INF settings (MSI opt-in)

On Windows 7, MSI/MSI-X allocation is typically controlled by INF registry keys under
`Interrupt Management\\MessageSignaledInterruptProperties`. The in-tree `inf/aero_virtio_input.inf` opts in:

```inf
[AeroVirtioInput_InterruptManagement_AddReg]
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported,        0x00010001, 1
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit,  0x00010001, 8
```

Notes:
- `MessageNumberLimit` is a request; Windows may allocate fewer messages than requested.
- If MSI/MSI-X allocation fails (or the device has no MSI/MSI-X capability), Windows will provide an **INTx** interrupt resource.

For background, see [`docs/windows/virtio-pci-modern-interrupts.md`](../../../docs/windows/virtio-pci-modern-interrupts.md) (§5).

#### Expected vector mapping

When MSI/MSI-X is active and Windows grants enough messages, the in-tree driver uses:

- **Vector/message 0:** virtio **config** interrupt
- **Vector/message 1:** queue 0 (`eventq`)
- **Vector/message 2:** queue 1 (`statusq`)

If Windows grants fewer than `1 + numQueues` messages, the driver falls back to:

- **All sources on vector/message 0** (config + all queues)

#### Troubleshooting / verifying which interrupt mode you got

- **Device Manager → Properties → Resources**:
  - INTx usually shows a small IRQ number (often shared).
  - MSI/MSI-X often shows a very large IRQ number (e.g. `42949672xx`) and may show multiple IRQ entries.
- **`aero-virtio-selftest.exe` markers**:
  - The selftest logs to `C:\\aero-virtio-selftest.log` and emits `AERO_VIRTIO_SELFTEST|TEST|virtio-input|...` markers on stdout/COM1.
  - The selftest also emits a `virtio-input-irq|INFO|...` line indicating which interrupt mode Windows assigned:
    - `virtio-input-irq|INFO|mode=intx`
    - `virtio-input-irq|INFO|mode=msi|messages=<n>` (message-signaled interrupts; MSI/MSI-X)
  - Newer selftest binaries also emit a dedicated marker with **driver-observed MSI-X routing details** (via a diagnostics IOCTL):
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS/FAIL/SKIP|mode=<intx|msix|unknown>|messages=<n>|mapping=...|used_vectors=<n>|config_vector=<n\|none>|queue0_vector=<n\|none>|queue1_vector=<n\|none>|...`
    - This is informational by default; to make MSI-X a hard requirement:
      - Guest-side (fail-fast): `aero-virtio-selftest.exe --require-input-msix` (or env var `AERO_VIRTIO_SELFTEST_REQUIRE_INPUT_MSIX=1`)
        - If provisioning the guest via `drivers/windows7/tests/host-harness/New-AeroWin7TestImage.ps1`, bake this into the
          scheduled task with `-RequireInputMsix`.
      - Host-side: `Invoke-AeroVirtioWin7Tests.ps1 -RequireVirtioInputMsix` *(alias: `-RequireInputMsix`)* /
        `invoke_aero_virtio_win7_tests.py --require-virtio-input-msix` *(alias: `--require-input-msix`)*
  - To request a larger MSI-X table size under QEMU in the in-tree harness (requires QEMU virtio `vectors` property),
    run the host harness with:
    `-VirtioMsixVectors N` / `--virtio-msix-vectors N` (global) or `-VirtioInputVectors N` / `--virtio-input-vectors N`
    (virtio-input only).
  - See `../tests/guest-selftest/README.md` for how to build/run the tool.

## Testing (in-tree harness)

End-to-end input report delivery is validated by the Win7 harness when enabled:

- Provision the guest selftest with `--test-input-events`.
- Run the host harness with `-WithInputEvents` / `--with-input-events`.
- Expected guest markers:
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS|...`
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|PASS|devices=<n>` (validates the underlying PCI function(s) are bound to `aero_virtio_input` and have no PnP/ConfigManager errors)
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS|...`

Optional: also validate scroll wheel + horizontal wheel end-to-end:

- Run the host harness with:
  - PowerShell: `-WithInputWheel` (alias: `-WithVirtioInputWheel`)
  - Python: `--with-input-wheel` (aliases: `--with-virtio-input-wheel`, `--require-virtio-input-wheel`, `--enable-virtio-input-wheel`)
- Expected guest marker:
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|PASS|...`

Optional: extended input event shapes (modifiers/buttons/wheel) end-to-end:

- Provision the guest selftest with `--test-input-events-extended` (or env var `AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_EXTENDED=1`).
- Run the host harness with `-WithInputEventsExtended` / `--with-input-events-extended`.
- Expected guest markers:
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|PASS|...`
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|PASS|...`
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|PASS|...`

Tablet (absolute pointer) report delivery can also be validated end-to-end:

- Install the tablet INF (`inf/aero_virtio_tablet.inf`) so `virtio-tablet-pci` binds to the driver.
- Provision the guest selftest with `--test-input-tablet-events` (alias: `--test-tablet-events`).
- Run the host harness with `-WithTabletEvents` / `--with-tablet-events`.
- Expected guest marker:
  - `AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|PASS|...`

To attach a `virtio-tablet-pci` device **without** QMP injection / marker enforcement (for example to validate
enumeration only), run the host harness with:

- PowerShell: `-WithVirtioTablet`
- Python: `--with-virtio-tablet`

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
powershell -ExecutionPolicy Bypass -File .\scripts\stage-built-sys.ps1 -Arch x64
```

If you built via the CI/MSBuild pipeline (which places outputs under `out/drivers/...`), run from the repo root:

```powershell
powershell -ExecutionPolicy Bypass -File drivers/windows7/virtio-input/scripts/stage-built-sys.ps1 `
  -Arch x64 `
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
inf\aero_virtio_tablet.cat
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
- If `inf\aero_virtio_tablet.cat` is present (tablet INF staged): also sign `inf\aero_virtio_tablet.cat`

## Verifying signatures (SYS + CAT)

From `drivers/windows7/virtio-input/`:

```cmd
.\scripts\verify-signature.cmd
```

By default the script verifies the staged package under `inf\`. You can pass an alternate package directory as the first argument:

```cmd
.\scripts\verify-signature.cmd C:\path\to\driver-package
```

The script verifies:

- `aero_virtio_input.sys`
- `aero_virtio_input.cat`
- If present: `aero_virtio_tablet.cat`

The script exits non-zero if `signtool.exe` is not available in `PATH` or if any present file is unsigned/invalid.

## Installation

### Device Manager (“Have Disk…”)

1. Device Manager → right-click the virtio-input PCI device → **Update Driver Software**
2. **Browse my computer**
3. **Let me pick** → **Have Disk…**
4. Browse to `drivers/windows7/virtio-input/inf/`
5. Select the matching INF:
   - `aero_virtio_input.inf` (keyboard + mouse)
   - `aero_virtio_tablet.inf` (tablet / absolute pointer)

### pnputil (Windows 7)

Windows 7 includes `pnputil.exe` but with an older CLI.

From an elevated command prompt:

```cmd
pnputil -i -a C:\path\to\aero_virtio_input.inf
pnputil -i -a C:\path\to\aero_virtio_tablet.inf
```

If you are using the deterministic release ZIP produced by `scripts/package-release.ps1`, the extracted folder also includes:

- `INSTALL_CERT.cmd` (optional; installs `aero-virtio-input-test.cer` into `Root` + `TrustedPublisher`; requires elevation)
- `INSTALL_DRIVER.cmd` (runs `pnputil -i -a` for `aero_virtio_input*.inf` and `aero_virtio_tablet*.inf` when present; prefers the unified INF, otherwise uses the per-arch INF; requires elevation)

## Verifying the driver loaded

### Device Manager

- The device should move under **Human Interface Devices** (HIDClass).
- When the device exposes the Aero contract subsystem IDs, the two virtio-input PCI functions appear as:
  - **Aero VirtIO Keyboard**
  - **Aero VirtIO Mouse**
  - **Aero VirtIO Tablet Device** (tablet / absolute pointer)
- Driver details should show `aero_virtio_input.sys`.

### Service state

```cmd
sc query aero_virtio_input
```

### Driver file present

```cmd
dir %SystemRoot%\System32\drivers\aero_virtio_input.sys
```

## Registry parameters (optional)

The driver reads optional configuration values from:

`HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters`

All values are `REG_DWORD`.

| Value | Default | Meaning |
| --- | --- | --- |
| `DiagnosticsMask` | `0x0000000F` (DBG builds) | Enables diagnostic logging when the driver is built with diagnostics (`VIOINPUT_DIAGNOSTICS==1`). Set to `0` to disable all logging. See `src/log.h` for bit definitions. In diagnostics builds, this can also be changed at runtime via `IOCTL_VIOINPUT_SET_LOG_MASK` (e.g. `hidtest.exe --set-log-mask 0x...`; add `--json` for machine-readable output). |
| `StatusQDropOnFull` | `0` | Debug knob for the virtio status queue (used for keyboard LED writes). When nonzero, pending statusq writes are dropped when the virtqueue is full. |

Most changes take effect the next time the driver is started (reboot or disable/enable the device). In diagnostics builds, `DiagnosticsMask` can also be changed at runtime via IOCTLs (see above).

### Debugging `StatusQDropOnFull`

To enable drop-on-full (elevated cmd):

```cmd
reg add HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters ^
  /v StatusQDropOnFull /t REG_DWORD /d 1 /f
```

Then reboot (or disable/enable the device). With the included `hidtest.exe`, you can generate many LED writes and observe drops via:

- `hidtest.exe --keyboard --state` (confirm `StatusQDropOnFull` is enabled, and that `StatusQActive=1` / `KeyboardLedSupportedMask!=0` so LED output is actually enabled)
  - Machine-readable: `hidtest.exe --keyboard --state --json` (or `--state-json`)
- `hidtest.exe --keyboard --reset-counters` (start from a clean monotonic-counter baseline; requires write access, rerun elevated if needed)
- `hidtest.exe --keyboard --led-cycle` (cycle the 5 HID boot keyboard LED bits: Num/Caps/Scroll/Compose/Kana)
- `hidtest.exe --keyboard --led-spam 10000`
- `hidtest.exe --keyboard --counters` (watch `LedWritesRequested` vs `LedWritesSubmitted`/`StatusQSubmits`, `StatusQCompletions`, and `StatusQFull`; with drop-on-full enabled also watch `VirtioStatusDrops` / `LedWritesDropped`)

Note: `LedWritesDropped` can also increase when LED output is disabled (`StatusQActive=0`, e.g. the device does not advertise `EV_LED` in `EV_BITS(types)`).

By default, `--led-spam` alternates the LED output report between `0` and `0x1F` (all 5 defined HID boot keyboard LED bits).
To spam a different “on” pattern, combine with `--led 0xMASK` (or `--led-hidd` / `--led-ioctl-set-output`).

## QEMU / emulator notes (expected device)

virtio-input appears as a **PCI virtio** function. In QEMU this is typically created with devices like:

- `virtio-keyboard-pci`
- `virtio-mouse-pci`
- `virtio-tablet-pci`

All of these use the virtio-input transport and should enumerate with `VEN_1AF4` and a virtio-input device ID.

- **Aero contract v1 / modern virtio-pci:** `DEV_1052` (what this in-tree driver package binds to)
- **Virtio transitional ID space:** `DEV_1011` (defined by the virtio spec, but **not** matched by the in-tree INFs)

For contract-v1 testing under QEMU, you typically want:

- `disable-legacy=on` (modern-only virtio-pci)
- `x-pci-revision=0x01` (so the device matches the `REV_01` contract major version / INF HWID gate)

Note: QEMU’s virtio-input devices typically report `ID_NAME` strings like `"QEMU Virtio Keyboard"`. This driver is
**strict by default** (Aero contract v1) and expects the Aero contract strings (`"Aero Virtio Keyboard"` /
`"Aero Virtio Mouse"` / `"Aero Virtio Tablet"`), but it also supports an **opt-in** compatibility mode:

```cmd
reg add HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters ^
  /v CompatIdName /t REG_DWORD /d 1 /f
```

Then reboot (or disable/enable the device). In compat mode the driver accepts QEMU `ID_NAME` strings, relaxes strict
`ID_DEVIDS` validation, and can infer device kind from `EV_BITS`. Compat mode does **not** relax the underlying Aero
transport checks (PCI IDs/revision, fixed BAR0 layout, queue sizing, etc).

Note: `virtio-tablet-pci` is an **absolute** pointing device (`EV_ABS`). It is supported by this driver, but binds via
the separate tablet INF (`inf/aero_virtio_tablet.inf`) and requires the device to advertise `ABS_X`/`ABS_Y`.
For Aero contract tablet devices (`ID_NAME="Aero Virtio Tablet"` / `SUBSYS_00121AF4`), strict mode also requires
`ABS_INFO` so coordinates can be scaled into the HID range. For non-contract tablets inferred via `EV_BITS`
(`EV_ABS` + `ABS_X`/`ABS_Y`), `ABS_INFO` is best-effort and the driver falls back to a default scaling range
(`0..32767`) if it is unavailable.

## Testing

- User-mode HID verification tool: `tools/hidtest/README.md`
- Manual QEMU test plan: `tests/qemu/README.md`
- Offline/slipstream install notes (DISM): `tests/offline-install/README.md`
- Common failure modes / troubleshooting: [`docs/troubleshooting.md`](./docs/troubleshooting.md)

## Release packaging (optional)

Once the driver binary exists, you can produce a deterministic, redistributable ZIP bundle using:

- `release/README.md`
- `scripts/package-release.ps1`

The packaged ZIP includes guest-side helper scripts (`INSTALL_CERT.cmd`, `INSTALL_DRIVER.cmd`) intended to be run from the extracted package directory on Windows 7.

## Known limitations
This driver intentionally implements a **minimal, contract-first** subset of virtio-input sufficient
for Windows 7 keyboard, mouse, and tablet (absolute pointer) input via the HID stack, plus a small set of optional
extensions that are implemented in-tree (consumer/media keys).

### Virtio-input functionality (guest-visible)

| Capability | Status | Notes (what Windows sees) |
| --- | --- | --- |
| Keyboard input (`EV_KEY` → HID keyboard, ReportID `1`) | **Supported** | Boot-protocol-style input report (8 modifiers + reserved + **6-key** array). Keys outside the translator’s Linux `KEY_*` → HID mapping are ignored. |
| Consumer/media keys (`EV_KEY` → HID Consumer Control, ReportID `3`) | **Supported** | Bitmask report exposed as a HID **Consumer Control** collection on the keyboard function. Supported keys: **Mute, Volume Down, Volume Up, Play/Pause, Next Track, Previous Track, Stop**. |
| Mouse relative motion (`EV_REL`: `REL_X`/`REL_Y`) | **Supported** | HID mouse report (ReportID `2`) with signed 8-bit X/Y deltas. Large deltas are split across multiple reports. |
| Mouse wheel (`EV_REL`: `REL_WHEEL`) | **Supported** | Vertical wheel (signed 8-bit). |
| Mouse horizontal wheel (`EV_REL`: `REL_HWHEEL`) | **Supported** | Mapped to HID **Consumer / AC Pan** (signed 8-bit). |
| Mouse buttons (`EV_KEY`: `BTN_*`) | **Supported** | 8-button HID bitmask (ReportID `2`, buttons 1–8). See mapping table below. |
| Keyboard LED output (Windows → driver → device) | **Supported** | HID output report (ReportID `1`) is translated to virtio-input `EV_LED` events on `statusq` (Num/Caps/Scroll + Compose/Kana bits). Device may ignore LED state per contract. |
| Tablet / absolute pointer (`EV_ABS` → HID absolute pointer, ReportID `4`) | **Supported** | Absolute X/Y are emitted as 16-bit values. For Aero contract tablets, coordinates are scaled using `ABS_INFO` min/max into the HID logical range (`0..32767`). For EV_BITS-inferred tablets and in compat mode, `ABS_INFO` is best-effort and the driver falls back to a default scaling range if it is unavailable. Installed via `inf/aero_virtio_tablet.inf` (Aero contract tablet HWID `SUBSYS_00121AF4`). Buttons/touch are supported when the device advertises `EV_KEY` button codes. |
| Multi-touch | **Not supported** | No multi-touch HID collections or contact tracking. |
| System control keys (power/sleep/wake) | **Not supported** | No HID System Control reports. |
| Force feedback (`EV_FF`) | **Not supported** | No force feedback / haptics support. |

INF note: contract tablet devices bind via `inf/aero_virtio_tablet.inf` (HWID `PCI\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01`).
`inf/aero_virtio_input.inf` is intentionally **SUBSYS-only**: it includes only the subsystem-qualified keyboard/mouse HWIDs
(`SUBSYS_0010`/`SUBSYS_0011`) for distinct Device Manager names, and does **not** include a strict generic (no `SUBSYS`)
fallback entry.

The strict generic fallback HWID (`PCI\VEN_1AF4&DEV_1052&REV_01`) is available only via the legacy alias INF
(`inf/virtio-input.inf.disabled` → rename to `virtio-input.inf`). When binding via the fallback entry, Device Manager will
show **Aero VirtIO Input Device**.

The tablet INF is more specific (`SUBSYS_0012...`), so it wins over the generic fallback when both packages are present and
the tablet subsystem ID is exposed. If the tablet INF is not installed (or the device does not expose the tablet subsystem
ID), the generic fallback entry (if enabled via the alias INF) can also bind to tablet devices.

Alias sync policy: outside the models sections (`[Aero.NTx86]` / `[Aero.NTamd64]`), from the first section header (`[Version]`)
onward, the alias INF is expected to match `inf/aero_virtio_input.inf` byte-for-byte (banner/comments may differ; see
`scripts/check-inf-alias.py`). It is allowed to diverge only in the models sections where it adds the opt-in fallback entry,
and enabling it **does** change HWID matching behavior. Do not ship/install it alongside `aero_virtio_input.inf` (install
only one INF basename at a time).

Device kind / report descriptor selection:

- `DeviceKind==Keyboard` exposes ReportID `1` (keyboard + LED output) and ReportID `3` (Consumer Control).
- `DeviceKind==Mouse` exposes ReportID `2` (mouse).
- `DeviceKind==Tablet` exposes ReportID `4` (tablet / absolute pointer).

`DeviceKind` is primarily derived from virtio `ID_NAME` and cross-checked against the PCI subsystem device ID when present.
In compat mode (`CompatIdName=1`), the driver also accepts common QEMU `ID_NAME` strings and may infer the kind from `EV_BITS`
when `ID_NAME` is unrecognized. For tablet/absolute-pointer devices, the driver can also fall back to `EV_BITS` inference
(`EV_ABS` + `ABS_X`/`ABS_Y`) even when compat mode is disabled, as long as the PCI subsystem device ID does **not** indicate an
Aero contract kind (`0x0010`/`0x0011`/`0x0012`). See `docs/virtio-input-notes.md` for details.

#### Mouse button mapping (`EV_KEY` → HID buttons 1–8)

The mouse report (ReportID `2`) exposes an 8-button bitmask (buttons 1–8 → bits 0–7):

| virtio-input `EV_KEY` code | HID button | Notes |
| --- | --- | --- |
| `BTN_LEFT` | Button 1 | |
| `BTN_RIGHT` | Button 2 | |
| `BTN_MIDDLE` | Button 3 | |
| `BTN_SIDE` | Button 4 | |
| `BTN_EXTRA` | Button 5 | |
| `BTN_FORWARD` | Button 6 | |
| `BTN_BACK` | Button 7 | |
| `BTN_TASK` | Button 8 | |
| `BTN_TOUCH` | Button 1 | Touch contact is treated as “left click”. |

> Driver model note: the INF installs the driver as a **KMDF HID minidriver** under `HIDClass`
> (Windows sees standard “HID Keyboard Device” / “HID-compliant mouse” collections; the keyboard function also exposes a “Consumer Control” collection).

### Contract / device-model constraints (AERO-W7-VIRTIO v1)

The driver and INF are intentionally strict and are **not** intended to be “generic virtio-input”:

| Constraint | Status | Where enforced |
| --- | --- | --- |
| Aero contract major version | **v1 only** (`REV_01`) | INF HWID match (`&REV_01`) + runtime check in `src/device.c` |
| Virtio-input PCI Device ID | **`DEV_1052` only** | INF HWID match + runtime device-id allowlist (`0x1052`) |
| Transitional / legacy virtio-input (`DEV_1011`) | **Unsupported** | Not matched by INF; rejected by runtime checks |
| Fixed BAR0 virtio-pci modern layout (contract v1) | **Required** | `VirtioPciModernValidateAeroContractV1FixedLayout` in `src/device.c` (expects BAR0 `len >= 0x4000`, caps at offsets `0x0000/0x1000/0x2000/0x3000`, `notify_off_multiplier = 4`) |
| Required virtqueues | **2 queues** (`eventq` + `statusq`) | `src/device.c` (expects 64/64 and `queue_notify_off` of `0/1`) |
| Virtqueue/ring feature negotiation | **Split ring only** | `src/device.c` requires `VIRTIO_F_VERSION_1` + `VIRTIO_F_RING_INDIRECT_DESC` and refuses to negotiate `VIRTIO_F_RING_EVENT_IDX` (no EVENT_IDX / packed rings in contract v1). |
| Required advertised event types/codes (`EV_BITS`) | **Required** | `src/device.c` enforces a minimum `EV_BITS` subset per device kind to fail fast on misconfigured devices. The **normative** Aero device-model requirements are defined in `docs/windows7-virtio-driver-contract.md` (§3.3.5). In strict contract mode, Aero tablet (`EV_ABS`) devices require `ABS_X/ABS_Y` and `ABS_INFO` ranges; for EV_BITS-inferred tablets and in compat mode, `ABS_INFO` is best-effort. |
| Device identification strings | **Required (strict by default)** | `src/device.c` enforces Aero `ID_NAME` strings + contract `ID_DEVIDS` and cross-checks them against the PCI subsystem device ID when present. Opt-in compat mode (`CompatIdName=1`) accepts common QEMU `ID_NAME` strings, relaxes `ID_DEVIDS`, and can infer kind from `EV_BITS`. |

### QEMU compatibility expectations

This driver is built for the **Aero contract v1** device model (`AERO-W7-VIRTIO`) and is strict by default.

If you're testing against stock QEMU virtio-input devices (which usually report `ID_NAME` strings like
`"QEMU Virtio Keyboard"`), enable compat mode in the guest (or build the driver with `AERO_VIOINPUT_COMPAT_ID_NAME=1`):

```cmd
reg add HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters ^
  /v CompatIdName /t REG_DWORD /d 1 /f
```

Then reboot (or disable/enable the device). Compat mode accepts QEMU `ID_NAME` strings, relaxes strict `ID_DEVIDS`
validation, and may infer device kind from `EV_BITS`. It does **not** relax the underlying transport checks that the
driver enforces at runtime (PCI IDs + `REV_01`, fixed BAR0 layout, and 2×64 virtqueues).

Under QEMU, you typically also need `disable-legacy=on,x-pci-revision=0x01` for the device to bind and start (INF gates
the Aero contract major version via `REV_01`).

Also note that stock QEMU virtio-input devices typically expose different (non-Aero) PCI subsystem IDs (or none at all).
The canonical keyboard/mouse INF (`inf/aero_virtio_input.inf`) is intentionally **SUBSYS-only**, so devices without the Aero
subsystem IDs will not bind using the canonical INF.

If you need to bind in an environment that does not expose/recognize the Aero subsystem IDs, you can either:

- **Emulate the subsystem IDs** to the contract values (`SUBSYS_0010` / `SUBSYS_0011` / `SUBSYS_0012`), or
- **Opt into strict generic fallback binding** by enabling the legacy alias INF (`inf/virtio-input.inf.disabled` → rename to
  `virtio-input.inf`), which adds the strict revision-gated generic fallback match (`PCI\VEN_1AF4&DEV_1052&REV_01`, no
  `SUBSYS`).

When binding via this fallback entry, Device Manager will show the generic **Aero VirtIO Input Device** rather than distinct
keyboard/mouse names.

Tablet devices bind via `inf/aero_virtio_tablet.inf` when that INF is installed. The tablet HWID is more specific
(`SUBSYS_0012...`), so it wins over the generic fallback when both packages are present. If the tablet INF is not installed
(or the device does not expose the tablet subsystem ID), the generic fallback entry (if enabled via the alias INF) can also
bind to tablet devices.

The legacy filename alias INF (`inf/virtio-input.inf.disabled`) exists for compatibility with workflows/tools that still look
for `virtio-input.inf`, and for opt-in generic fallback binding.

Alias sync policy: it is allowed to diverge from `inf/aero_virtio_input.inf` only in the models sections
(`[Aero.NTx86]` / `[Aero.NTamd64]`) where it adds the strict generic fallback match. Outside those models sections, from the
first section header (`[Version]`) onward, it is expected to match byte-for-byte (banner/comments may differ; see
`scripts/check-inf-alias.py`). Enabling it **does** change HWID matching behavior.

Do not ship/install it alongside `aero_virtio_input.inf`: overlapping bindings can lead to confusing driver selection (ship
only one INF basename at a time).

Unknown subsystem IDs are allowed by the driver; device-kind classification still follows the `ID_NAME`/`EV_BITS` rules
described above.

For authoritative PCI-ID and contract rules, see:

- `docs/pci-hwids.md`
- `../../../docs/windows7-virtio-driver-contract.md`

## Power management notes (Win7 HID idle)

Windows 7's `HIDCLASS.SYS` may send `IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST` (a **METHOD_NEITHER** IOCTL) to enable HID
idle/selective-suspend behavior. The driver handles this request by **completing it immediately with `STATUS_SUCCESS`**
and **does not dereference any caller-provided pointers**.

This avoids `STATUS_NOT_SUPPORTED` during enumeration and allows the HID stack to manage device idle/sleep transitions
using the driver's existing D0Entry/D0Exit reset-report behavior as the baseline.
