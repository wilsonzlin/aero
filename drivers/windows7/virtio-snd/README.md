<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Windows 7 virtio-snd driver package (build + signing)

This directory contains the Aero **virtio-snd** Windows 7 SP1 driver sources, plus scripts to produce an **installable, test-signed** driver package.

The intended developer workflow is:

1. Build `aero_virtio_snd.sys`
2. Copy it into `drivers/windows7/virtio-snd/inf/`
3. Generate a test certificate, generate a catalog (`.cat`), and sign `SYS + CAT`
4. Install on Windows 7 with test-signing enabled (Device Manager → “Have Disk…”)

## Interrupts: INTx baseline, optional MSI/MSI-X

Per the [`AERO-W7-VIRTIO` v1 contract](../../../docs/windows7-virtio-driver-contract.md) (§1.8), **INTx is required** and MSI/MSI-X is an optional enhancement.
MSI/MSI-X must not be required for functionality: if Windows does not allocate MSI/MSI-X, the driver is expected to fall back to INTx.

### Enabling MSI/MSI-X (INF)

On Windows 7, message-signaled interrupts are typically enabled through INF registry settings under the device’s hardware key:

```inf
[AeroVirtioSnd_Install.NT.HW]
AddReg = AeroVirtioSnd_InterruptManagement_AddReg, AeroVirtioSnd_Parameters_AddReg

[AeroVirtioSnd_InterruptManagement_AddReg]
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported,        0x00010001, 1
; virtio-snd needs config + 4 queues = 5 vectors; request a little extra for future growth.
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit,  0x00010001, 8

; Per-device bring-up toggles (defaults):
[AeroVirtioSnd_Parameters_AddReg]
HKR,Parameters,,0x00000010
HKR,Parameters,ForceNullBackend,0x00010003,0
HKR,Parameters,AllowPollingOnly,0x00010003,0
```

Notes:
- `MessageNumberLimit` is a request; Windows may allocate fewer messages than requested.
- `0x00010001` = `REG_DWORD`
- `0x00010003` = `REG_DWORD` + `FLG_ADDREG_NOCLOBBER` (do not overwrite an existing value; preserves per-device bring-up toggles across reinstalls/upgrades).
- If MSI/MSI-X allocation fails (or the device has no MSI/MSI-X capability), Windows will provide an **INTx** interrupt resource.
- If you modify the INF, regenerate the catalog and re-sign the package (required on Win7 x64 unless test-signing is enabled).

For background, see [`docs/windows/virtio-pci-modern-interrupts.md`](../../../docs/windows/virtio-pci-modern-interrupts.md) (§5).

### Expected vector mapping

When MSI/MSI-X is active and Windows grants enough messages, the expected mapping is:

- **Vector/message 0:** virtio **config** interrupt (`common_cfg.msix_config`)
- **Vector/message 1..4:** queues 0..3 (`controlq`, `eventq`, `txq`, `rxq`)

If Windows grants fewer than `1 + numQueues` messages, the driver falls back to:

- **All sources on vector/message 0** (config + all queues)

### Troubleshooting / verifying which interrupt mode you got

- **Device Manager → Properties → Resources**:
  - INTx usually shows a small IRQ number (often shared).
  - MSI/MSI-X often shows a very large IRQ number (e.g. `42949672xx`) and may show multiple IRQ entries.
- **Kernel debug output (DbgPrintEx)**:
  - During `START_DEVICE`, the driver prints an always-on line indicating which interrupt mode was selected:
    - `virtiosnd: interrupt mode: MSI/MSI-X ...`
    - `virtiosnd: interrupt mode: INTx`
    - `virtiosnd: interrupt mode: polling-only`
  - You can view this output with a kernel debugger or Sysinternals **DebugView** (Capture Kernel).
- **`aero-virtio-selftest.exe` markers**:
  - The selftest logs to `C:\\aero-virtio-selftest.log` and emits `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|...` markers on stdout/COM1.
  - The selftest also emits a `virtio-snd-irq|INFO|...` line describing the observed interrupt mode:
    - `virtio-snd-irq|INFO|mode=intx|...`
    - `virtio-snd-irq|INFO|mode=msix|messages=<n>|msix_config_vector=0x....|msix_queue0_vector=0x....|...`
      - When the optional `\\.\aero_virtio_snd_diag` interface is available, this line also includes:
        - `msix_queue1_vector..msix_queue3_vector` (per-queue MSI-X routing)
        - `interrupt_count`, `dpc_count`, and `drain0..drain3` counters (diagnostics)
    - `virtio-snd-irq|INFO|mode=none|...` (polling-only; no interrupt objects are connected)
    - (On older images/drivers where the diag interface is unavailable, the selftest falls back to best-effort PnP
      resource inspection and may report `mode=msi` with only a message count.)
  - See `../tests/guest-selftest/README.md` for how to build/run the tool.

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

## Optional/Compatibility Features

This section documents behavior that is **not required by AERO-W7-VIRTIO contract v1**, but is relevant when running
against non-contract virtio-snd implementations (for example, stock QEMU).

### `eventq` handling (virtio-snd asynchronous notifications)

Contract v1 reserves `eventq` for future use and forbids drivers from depending on it
(`docs/windows7-virtio-driver-contract.md` §3.4.2.1).

Driver behavior:

- The driver still initializes `eventq` and posts a small bounded pool of writable buffers.
- If the device completes `eventq` buffers, the driver drains/reposts them and (best-effort) parses/logs known event
  types (jack connect/disconnect, period elapsed, XRUN, control notify).
- When the PortCls **WaveRT** miniport is running with the virtio backend, it also registers a best-effort eventq
  callback to integrate virtio-snd PCM notifications into the WaveRT pipeline:
  - `PCM_PERIOD_ELAPSED`: queues the WaveRT period DPC as an additional wakeup source (the periodic WaveRT timer remains
    active as a fallback for contract-v1 devices that emit no events). Playback ticks are coalesced so timer-driven and
    event-driven wakeups do not double-advance `PacketCount`.
  - `PCM_XRUN`: schedules a coalesced PASSIVE_LEVEL recovery work item that re-issues `STOP/START` for the affected
    stream(s). For playback, the submission cursor is realigned to the current play position so the next tick can
    re-prime a small lead of audio cleanly.
- The driver maintains per-boot `eventq` counters (completions/parsed/period/xrun/etc) and exposes them via:
  - a structured teardown log marker: `AERO_VIRTIO_SND_EVENTQ|...`
  - the guest selftest marker: `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|INFO|...`
- Audio streaming (render/capture) must remain correct even if `eventq` is absent, silent, or noisy.

How to validate (in-tree harness):

- The functional harness (`drivers/windows7/tests/`) validates that playback + capture + duplex are correct under QEMU
  (via `aero-virtio-selftest.exe`) when `-WithVirtioSnd` / `--with-virtio-snd` is enabled.
- For eventq-specific debugging, use the `DebugLogs` build (`aero_virtio_snd_dbg.sys`) and capture kernel debug output
  (e.g. WinDbg/DebugView) while exercising playback/capture; look for `virtiosnd: eventq:` log lines.

### MSI/MSI-X interrupts

Contract v1 requires INTx, but MSI/MSI-X is supported as an optional enhancement when Windows grants message interrupts
(the shipped INF opts in; see “Interrupts: INTx baseline, optional MSI/MSI-X” above).

How to validate (in-tree harness):

- Request a larger MSI-X table size from QEMU (requires QEMU virtio `vectors` property):
  - global: `-VirtioMsixVectors N` / `--virtio-msix-vectors N`
  - virtio-snd only: `-VirtioSndVectors N` / `--virtio-snd-vectors N`
- Optionally fail the harness if MSI-X is not enabled on the device: `-RequireVirtioSndMsix` / `--require-virtio-snd-msix`.
- Inspect guest diagnostics (`virtio-snd-irq|INFO|mode=...`) and the mirrored host marker (`AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_IRQ_DIAG|...`).
  - When `-RequireVirtioSndMsix` / `--require-virtio-snd-msix` is enabled, the harness also requires the guest marker
    `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=msix|...` so the effective interrupt mode is validated end-to-end.

### Multi-format/device capability variance (non-contract)

Contract v1 fixes the required PCM formats/rates and stream topology (stream 0 render + stream 1 capture, both 48kHz
S16_LE).

For compatibility, the bring-up path is defensive when a device advertises a superset:

- It validates that the required contract format/rate/channel combinations are present in `PCM_INFO`.
- When `PCM_INFO` capabilities are available, it dynamically generates WaveRT pin data ranges and can expose
  additional formats/rates/channels to the Windows 7 audio stack.
- It preserves the contract-v1 baseline format as the **first** enumerated format (so Windows keeps the
  expected default mix format).
- When `PCM_INFO` is unavailable (null backend / some legacy builds), the driver falls back to fixed contract-v1 formats.

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

Optional (DBG=1 / `VIRTIOSND_TRACE*` enabled) local debug-logging build outputs:

- `out/drivers/windows7/virtio-snd/x86/aero_virtio_snd_dbg.sys`
- `out/drivers/windows7/virtio-snd/x64/aero_virtio_snd_dbg.sys`

This uses the **DebugLogs** configuration in `aero_virtio_snd.vcxproj` (not built in CI):

```powershell
# From the repo root:
.\ci\build-drivers.ps1 -Configuration DebugLogs -Drivers windows7/virtio-snd
```

You can also build it directly with MSBuild (or from Visual Studio by selecting the
`DebugLogs` configuration):

```powershell
msbuild .\drivers\windows7\virtio-snd\aero_virtio_snd.vcxproj /m `
  /p:Configuration=DebugLogs /p:Platform=x64
```

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

For the DebugLogs build, the MSBuild output file is named `aero_virtio_snd_dbg.sys`, but the
canonical `inf/aero_virtio_snd.inf` references `aero_virtio_snd.sys`. When staging into `inf/`,
you must either:

- **Rename** `aero_virtio_snd_dbg.sys` → `aero_virtio_snd.sys` when copying, or
- Use `scripts/stage-built-sys.ps1 -Variant debuglogs`, which performs the rename automatically, or
- Create a new INF that references `aero_virtio_snd_dbg.sys` (and ideally uses a distinct service
  name) if you want to keep both binaries in the same staging folder.

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

For the DebugLogs (DBG=1) build output (`aero_virtio_snd_dbg.sys`), stage it as the canonical INF
name (`aero_virtio_snd.sys`) with:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\stage-built-sys.ps1 -Arch amd64 -Variant debuglogs
```

For the optional transitional/QEMU package:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\stage-built-sys.ps1 -Arch amd64 -Variant legacy
```

To build a signed `release/` package in one step (stages SYS → Inf2Cat → sign → package):

```powershell
# Contract v1 (default):
powershell -ExecutionPolicy Bypass -File .\scripts\build-release.ps1 -Arch both -InputDir <build-output-root>

# DebugLogs (DBG=1 / VIRTIOSND_TRACE* enabled):
powershell -ExecutionPolicy Bypass -File .\scripts\build-release.ps1 -Arch both -Variant debuglogs -InputDir <build-output-root>

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

## Bring-up toggles (registry)

For diagnostics / bring-up, the driver exposes per-device registry toggles (**debug/dev only**):

- `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend` (`REG_DWORD`)
  - Default: `0` (use virtio backend; bring-up failures surface as Code 10)
  - Set to `1` to force the silent “null” backend, allowing the PortCls/WaveRT stack to start even if virtio transport bring-up fails.
- `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\AllowPollingOnly` (`REG_DWORD`)
  - Default: `0` (interrupt-driven; requires at least one usable interrupt resource — MSI/MSI-X preferred with INTx fallback; fails `START_DEVICE` if neither MSI/MSI-X nor INTx can be connected)
  - Set to `1` to allow the driver to start even when no usable interrupt can be discovered/connected. In this mode the driver relies on periodic used-ring polling (driven by the WaveRT period timer DPC).
  - Applies to the modern virtio-pci transport packages (`aero_virtio_snd.sys` and `virtiosnd_legacy.sys`); the legacy I/O-port bring-up package does not use this toggle.

Find `<DeviceInstancePath>` via **Device Manager → device → Details → “Device instance path”**.

After changing a toggle value, reboot the guest or disable/enable the device so Windows re-runs `START_DEVICE`.

Backwards compatibility note: older installs may have these values under the per-device driver key (the software key for the device/driver instance). The driver checks the per-device `Device Parameters` key first and falls back to the driver key.

Example (elevated `cmd.exe`, replace `<DeviceInstancePath>`):

```cmd
reg add "HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters" /v ForceNullBackend /t REG_DWORD /d 1 /f
```

To enable polling-only mode (modern virtio-pci transport packages only):

```cmd
reg add "HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters" /v AllowPollingOnly /t REG_DWORD /d 1 /f
```

To verify the current values (elevated `cmd.exe`, replace `<DeviceInstancePath>`):

```cmd
reg query "HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters" /v ForceNullBackend
reg query "HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters" /v AllowPollingOnly
```

## Offline / slipstream installation (optional)

If you want virtio-snd to bind automatically on first boot (for example when building unattended Win7 images), see:

- `tests/offline-install/README.md`

## Manual QEMU test plan
  
For a repeatable manual bring-up/validation plan under QEMU, see:

- `tests/qemu/README.md`

## INF linter (Linux/macOS)

To catch accidental drift in the virtio-snd INFs (HWIDs, KS/WDMAudio + KS interface registration, install/service wiring, and legacy alias INF sync), run:

```sh
cd drivers/windows7/virtio-snd
./scripts/lint-inf.sh
```

This script validates:

- The **contract v1** package (`inf/aero_virtio_snd.inf`) and its optional filename alias (`inf/virtio-snd.inf.disabled`)
- The optional **transitional/QEMU** package (`inf/aero-virtio-snd-legacy.inf`)
- The optional **legacy I/O-port** bring-up package (`inf/aero-virtio-snd-ioport.inf`)

## Host unit tests (host builds)

Kernel drivers cannot run in CI, but parts of the virtio-snd protocol engines can
be compiled and unit tested on the host (descriptor/SG building, framing, and
status/state handling).

Prerequisites:

- CMake in `PATH` (`cmake` + `ctest`).
- A C compiler toolchain:
  - On Linux/macOS, Clang/GCC should work.
  - On Windows, Visual Studio / “Build Tools for Visual Studio” (MSVC) is recommended.
    - Run from a “Developer PowerShell/Command Prompt for VS” so `cl.exe` is available.
    - Ninja is optional.
- On Windows, PowerShell 7+ (`pwsh`) or Windows PowerShell 5.1 (`powershell.exe`). If script execution is blocked, use
  `-ExecutionPolicy Bypass` (or `Set-ExecutionPolicy -Scope Process Bypass`).

### Full host-buildable suite (recommended)

Build the top-level CMake project at `drivers/windows7/virtio-snd/tests/`. This is the
**superset** and includes:

- `virtiosnd_sg_tests`
- `virtiosnd_proto_tests` (integrated tests that compile a subset of `src/*.c`)
- everything under `drivers/windows7/virtio-snd/tests/host/` (added as a subdirectory)

From the repo root:

```sh
./scripts/run-host-tests.sh
```

On Windows:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1
```

Replace `pwsh` with `powershell.exe` if you are using Windows PowerShell.

To force a clean rebuild:

```sh
./scripts/run-host-tests.sh --clean
```

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -Clean
```

The default build directory is `out/virtiosnd-tests`. Override with:

```sh
./scripts/run-host-tests.sh --build-dir out/my-virtiosnd-tests
```

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -BuildDir out\my-virtiosnd-tests
```

Multi-config generators (Visual Studio, Ninja Multi-Config) require a build/test configuration.
`run-host-tests.ps1` auto-detects multi-config build dirs and uses `-Configuration` (default:
`Release`):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -Configuration Debug
```

Note: `-Configuration` is only used when the selected CMake generator is multi-config (for example
Visual Studio). For single-config generators (Ninja/Makefiles), the script configures
`CMAKE_BUILD_TYPE=Release`; to do a Debug build in that mode, configure/build manually with
`-DCMAKE_BUILD_TYPE=Debug`.

Troubleshooting (Windows):

- If you see “`cl.exe` not found” / CMake cannot compile, open a **Developer PowerShell/Command Prompt
  for VS** (so MSVC environment variables are set).
- To force a specific CMake generator, set `CMAKE_GENERATOR` and re-run with `-Clean`:
  - PowerShell: `$env:CMAKE_GENERATOR = 'Ninja'` or `$env:CMAKE_GENERATOR = 'Visual Studio 17 2022'`
- If script execution is blocked by policy, use `-ExecutionPolicy Bypass` (as shown) or run
  `Set-ExecutionPolicy -Scope Process Bypass`.

Or run directly:

```sh
cmake -S drivers/windows7/virtio-snd/tests -B out/virtiosnd-tests
cmake --build out/virtiosnd-tests
ctest --test-dir out/virtiosnd-tests --output-on-failure
```

### Subset: `tests/host` only (fast iteration)

The `drivers/windows7/virtio-snd/tests/host/` project can still be built standalone when
you only want that smaller subset of tests (it is also included when building the full
suite above):

> Note: this subset does **not** build `virtiosnd_proto_tests`. Use the full suite above
> when you want integrated coverage of the protocol engines compiled from `src/*.c`.

```sh
./scripts/run-host-tests.sh --host-only
```

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -HostOnly
```

The default build directory for `--host-only` is `out/virtiosnd-host-tests`. Override with:

```sh
./scripts/run-host-tests.sh --host-only --build-dir out/my-virtiosnd-host-tests
```

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -HostOnly -BuildDir out\my-virtiosnd-host-tests
```

Or run directly:

```sh
cmake -S drivers/windows7/virtio-snd/tests/host -B out/virtiosnd-host-tests
cmake --build out/virtiosnd-host-tests
ctest --test-dir out/virtiosnd-host-tests --output-on-failure
```

Note: for multi-config generators (Visual Studio, Ninja Multi-Config):

- For the PowerShell runner, use `-Configuration <cfg>` (default: `Release`).
- When invoking CMake/CTest directly, add:
  - `--config <cfg>` to `cmake --build`
  - `-C <cfg>` to `ctest`
- The Bash helper script (`scripts/run-host-tests.sh`) does not currently accept a configuration
  parameter; use the direct commands above if you configured a multi-config generator.

## Release packaging (optional)

Once the package has been built/signed, you can stage a Guest Tools–ready folder under `release\<arch>\virtio-snd\` using:

- `scripts/package-release.ps1` (see `release/README.md`)

The same script can also produce a deterministic ZIP bundle from `inf/` by passing `-Zip`.
