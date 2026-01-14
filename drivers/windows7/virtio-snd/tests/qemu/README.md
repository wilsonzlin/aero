<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# QEMU manual test plan: Windows 7 virtio-snd (PortCls/WaveRT) driver

This document describes a repeatable way to manually validate a Windows 7 **virtio-snd** audio driver end-to-end under **QEMU**.

What this test plan verifies:

1. QEMU exposes a virtio-snd PCI function with a stable hardware ID (HWID)
2. Windows 7 binds the virtio-snd driver package (INF/SYS) to that PCI function
3. The Windows audio stack enumerates a **render** endpoint (Control Panel → Sound)
4. Audio playback works (audible on the host, or captured to a WAV file in headless runs)
5. The Windows audio stack enumerates a **capture** endpoint (Control Panel → Sound → Recording)
6. Audio capture works (records host input if available; may record silence if no input source is available)
7. The **virtio-snd** portion of the guest audio smoke test passes (selftest **Task 128**)

> Note: virtio-snd capture depends on the host/QEMU audio backend providing an input
> source. If none is available, the capture stream will record silence, but
> endpoint enumeration should still work.
>
> The current Aero Windows 7 virtio-snd driver package exposes both **render** (stream id `0`, `txq`)
> and **capture** (stream id `1`, `rxq`) endpoints per `AERO-W7-VIRTIO` v1.

References:

- PCI ID/HWID reference: `../../docs/pci-hwids.md`
- Optional: offline/slipstream staging so Windows binds the driver on first boot:
  - `../offline-install/README.md`

## Prerequisites

- A QEMU build new enough to expose a virtio-snd PCI device.
  - Known-good reference: QEMU **8.2.x**.
  - For binding with the strict Aero contract v1 INF (`aero_virtio_snd.inf`), QEMU must support
    `disable-legacy=on` and `x-pci-revision=0x01` on the virtio-snd device
    (to match `PCI\VEN_1AF4&DEV_1059&REV_01`).
  - Verify supported properties with `qemu-system-x86_64 -device virtio-sound-pci,help`.
- A Windows 7 SP1 VM disk image (x86 or x64).
- A virtio-snd driver package directory staged next to the INF:
  - Repo layout (staging): `drivers/windows7/virtio-snd/inf/`
  - Guest Tools / CI bundle ZIP/ISO layout: `drivers\virtio-snd\x86\` or `drivers\virtio-snd\x64\`
  - **Preferred: strict Aero contract v1** (requires the contract-v1 HWID):
    - `inf/aero_virtio_snd.inf`
    - `aero_virtio_snd.sys` built for x86 or x64
  - **Optional: QEMU compatibility package** (for QEMU builds/configurations that cannot expose the contract-v1 HWID):
    - `inf/aero-virtio-snd-legacy.inf`
    - `virtiosnd_legacy.sys` built for x86 or x64 (MSBuild `Configuration=Legacy`)
    - Note: this compatibility package is **not** included in the default CI/Guest Tools driver bundle
      (see `ci-package.json`). Build/package it manually (`Configuration=Legacy`) if you need it.
  - **Optional: legacy I/O-port transport** (older bring-up; not part of `AERO-W7-VIRTIO` v1):
    - `inf/aero-virtio-snd-ioport.inf`
    - `virtiosnd_ioport.sys` built for x86 or x64 (MSBuild `virtio-snd-ioport-legacy.vcxproj`)
- Important: `aero_virtio_snd.inf` is revision-gated and binds only to the Aero contract v1 HWID
  (`PCI\VEN_1AF4&DEV_1059&REV_01`). If the device does not expose that HWID, Windows will not
  bind this package until you adjust QEMU device options (see below).
- Test signing enabled in the guest (or a properly production-signed driver package).

Optional (but recommended for headless hosts):

- A QEMU audio backend that does not require host audio hardware.
  - Recommended: `-audiodev wav,...` (captures guest audio to a host `.wav` file).

## Quick start: helper script

This directory includes a small helper script that builds a QEMU command line for you:

`drivers/windows7/virtio-snd/tests/qemu/run-virtio-snd.sh`

It probes your QEMU binary for virtio-snd device properties and enables the strict
contract-v1 identity (`disable-legacy=on,x-pci-revision=0x01`) when supported.
If the properties are missing, it falls back to QEMU’s default/transitional identity
and prints instructions to use the legacy INF (`aero-virtio-snd-legacy.inf`).

Examples (from repo root):

```bash
# x64 (default), capture guest playback to ./virtio-snd-x64.wav:
bash drivers/windows7/virtio-snd/tests/qemu/run-virtio-snd.sh --disk win7-x64.qcow2

# Print the final QEMU command line without running it:
bash drivers/windows7/virtio-snd/tests/qemu/run-virtio-snd.sh --print --arch x86
```

## Verify interrupt mode (MSI/MSI-X vs INTx)

The virtio-snd driver prefers **MSI/MSI-X** message-signaled interrupts when Windows provides
them, and falls back to **INTx** automatically.

During `START_DEVICE` the driver prints an always-on diagnostic line indicating which mode was
selected:

- `virtiosnd: interrupt mode: MSI/MSI-X (messages=..., all_on_vector0=...)`
- `virtiosnd: interrupt mode: INTx`
- `virtiosnd: interrupt mode: polling-only`

To view these logs in a Windows 7 guest:

- Attach a kernel debugger, or
- Use Sysinternals **DebugView** with **Capture Kernel** enabled.

If `aero-virtio-selftest.exe` is installed, it also emits a serial/stdout marker indicating which interrupt mode Windows assigned:

- `virtio-snd-irq|INFO|mode=intx`
- `virtio-snd-irq|INFO|mode=msix|messages=<n>|msix_config_vector=0x....|...` (when the driver exposes the optional `\\.\aero_virtio_snd_diag` interface)
- `virtio-snd-irq|INFO|mode=none|...` (polling-only; no interrupt objects are connected)
- `virtio-snd-irq|INFO|mode=msi|messages=<n>` (fallback: message interrupts; does not distinguish MSI vs MSI-X)

## Optional bring-up: polling-only mode (no interrupts)

If you are testing against an early/buggy virtio-snd device model where interrupts are not delivered or cannot be connected, the driver supports an **opt-in** polling-only mode.

Set the per-device registry value:

- `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\AllowPollingOnly` = `1` (`REG_DWORD`)

Then disable/enable the device (or reboot) so Windows re-runs `START_DEVICE`.

In this mode the driver relies on periodic used-ring polling (driven by the WaveRT period timer DPC) instead of ISR/DPC delivery.

Notes:

- This is intended for bring-up/debugging only; the default is still interrupt-driven.
- Most QEMU builds should not require this.

## Identify the virtio-snd device name in your QEMU build

QEMU device naming can vary by version/build. Always confirm what your QEMU binary calls the device:

```bash
qemu-system-x86_64 -device help | grep -E "virtio-(sound|snd)-pci"
```

Common outputs include:

- `virtio-sound-pci` (typical upstream name)
- `virtio-snd-pci` (alias on some builds)

If neither appears, upgrade QEMU.

## QEMU command lines

The examples below are intentionally explicit and can be used as a starting point. Adjust paths, CPU accel, and disk/network options as needed.

The audio backend uses QEMU `wav` so playback can be validated without relying on the host audio stack.

> Note: These command lines intentionally use an IDE boot disk and an e1000 NIC so you do not need any other virtio drivers installed just to boot Windows.

### Windows 7 SP1 x86

```bash
qemu-system-i386 \
  -machine pc,accel=kvm \
  -m 2048 \
  -cpu qemu32 \
  -drive file=win7-x86.qcow2,if=ide,format=qcow2 \
  -net nic,model=e1000 -net user \
  -audiodev wav,id=aerosnd0,path=virtio-snd-x86.wav \
  -device virtio-sound-pci,audiodev=aerosnd0,disable-legacy=on,x-pci-revision=0x01
```

### Windows 7 SP1 x64

```bash
qemu-system-x86_64 \
  -machine pc,accel=kvm \
  -m 4096 \
  -cpu qemu64 \
  -drive file=win7-x64.qcow2,if=ide,format=qcow2 \
  -net nic,model=e1000 -net user \
  -audiodev wav,id=aerosnd0,path=virtio-snd-x64.wav \
  -device virtio-sound-pci,audiodev=aerosnd0,disable-legacy=on,x-pci-revision=0x01
```

These examples validate the strict Aero contract v1 identity under QEMU (modern `PCI\VEN_1AF4&DEV_1059&REV_01`); install
`aero_virtio_snd.inf`.

If your QEMU build cannot expose **both** `disable-legacy=on` and `x-pci-revision=0x01`, the strict INF will not
bind. In that case, run virtio-snd in **transitional** mode (do not set `disable-legacy=on`, so the device
enumerates as `PCI\VEN_1AF4&DEV_1018`) and install the QEMU compatibility package instead (`aero-virtio-snd-legacy.inf`).

### Audio backend alternatives

If you cannot use the `wav` backend, replace `-audiodev wav,...` with a backend supported by your host:

- Linux (PulseAudio): `-audiodev pa,id=aerosnd0`
- Windows: `-audiodev dsound,id=aerosnd0`

## Aero contract v1 (virtio-snd PCI identity)

`AERO-W7-VIRTIO` v1 is **modern-only** and revision-gated:

- PCI vendor/device: `VEN_1AF4&DEV_1059`
- Contract major version: `REV_01` (encoded in PCI Revision ID)
- Transport: PCI vendor-specific capabilities + BAR0 MMIO (virtio-pci modern)
 
Some QEMU builds enumerate virtio-snd as a transitional device by default (often with `REV_00`);
use the opt-in legacy package in that case.

### Expected HWIDs (quick reference)

| Mode | Expected HWID | INF | SYS | Installed service |
|---|---|---|---|---|
| Aero contract v1 (modern-only; default build) | `PCI\VEN_1AF4&DEV_1059&REV_01` | `aero_virtio_snd.inf` | `aero_virtio_snd.sys` | `aero_virtio_snd` |
| QEMU transitional (optional) | `PCI\VEN_1AF4&DEV_1018` (transitional; see Device Manager for full list) | `aero-virtio-snd-legacy.inf` | `virtiosnd_legacy.sys` | `aeroviosnd_legacy` |
| QEMU transitional (I/O-port legacy; optional) | `PCI\VEN_1AF4&DEV_1018&REV_00` | `aero-virtio-snd-ioport.inf` | `virtiosnd_ioport.sys` | `aeroviosnd_ioport` |

The shipped contract INF (`inf/aero_virtio_snd.inf`) is intentionally strict and matches only:

- `PCI\VEN_1AF4&DEV_1059&REV_01`

For QEMU validation, you must:

- force modern-only mode (`disable-legacy=on`) and
- set the contract revision (`x-pci-revision=0x01`).

If your QEMU build cannot set these properties, use the opt-in QEMU compatibility package instead
(`inf/aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`).

Tip: confirm your QEMU build supports these properties with:

```bash
qemu-system-x86_64 -device virtio-sound-pci,help
```

Development note: the repo also contains an optional legacy filename alias INF
(`inf/virtio-snd.inf.disabled`). If you rename it back to `virtio-snd.inf`, it installs the same
driver/service (`aero_virtio_snd`) and binds the same contract-v1 HWIDs as `aero_virtio_snd.inf`, but provides the legacy filename
for compatibility with older tooling/workflows. The alias INF uses `CatalogFile = aero_virtio_snd.cat` and is
checked in disabled-by-default to avoid shipping multiple INFs that match the same HWIDs. This legacy INF is
not staged into the CI driver bundle.

## Verifying HWID in Device Manager

Before installing the driver (or when troubleshooting binding), confirm the device HWID that Windows sees:

1. In the Windows 7 VM, open **Device Manager**.
2. Find the virtio-snd device (before installation it may appear as an unknown “PCI Device” under **Other devices**).
3. Right-click → **Properties** → **Details** tab.
4. In the **Property** dropdown, select **Hardware Ids**.

Expected values include:

- `PCI\VEN_1AF4&DEV_1059&REV_01`
- `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01` (if the emulator/QEMU device reports Aero subsystem IDs)

Windows may also list less-specific forms (without `REV_01` / `SUBSYS_...`), but the Aero INF is
revision-gated; if you do not see a `...&REV_01` HWID, the driver will not bind.

Ensure the HWID matches the INF you intend to install (`aero_virtio_snd.inf` vs `aero-virtio-snd-legacy.inf`).
If you want to validate the strict Aero contract v1 package, configure QEMU to expose `PCI\VEN_1AF4&DEV_1059&REV_01`
(for example `disable-legacy=on,x-pci-revision=0x01` when supported).

## Preferred: automated host harness

For repeatable automated validation (including strict `AERO-W7-VIRTIO` v1 device configuration),
prefer the Windows 7 host harness. It probes QEMU for supported virtio-snd device properties (via
`-device virtio-sound-pci,help`) and enables contract-v1 identification when supported (for example
`disable-legacy=on` and `x-pci-revision=0x01`).

The automated Win7 host harness uses the same contract-v1 settings (`disable-legacy=on` and
`x-pci-revision=0x01`) when supported:

- `drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1`
- `drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py`

The harness configures **modern-only** virtio devices (it enables `disable-legacy=on` and forces
`x-pci-revision=0x01` when supported), which matches the default virtio-snd driver + INF contract.

To catch QEMU/device-arg misconfiguration early (for example, missing `x-pci-revision=0x01` resulting in `REV_00`),
the host harness also includes an optional host-side PCI ID preflight via QMP `query-pci`:

- PowerShell: `Invoke-AeroVirtioWin7Tests.ps1 -QemuPreflightPci` (alias: `-QmpPreflightPci`)
- Python: `invoke_aero_virtio_win7_tests.py --qemu-preflight-pci` (alias: `--qmp-preflight-pci`)

## Guest-side validation (Windows 7)

### 1) Enable test signing (if needed)

On Windows 7 x64, kernel drivers must be signed unless test signing / signature enforcement overrides are enabled.

From an elevated Command Prompt:

```bat
bcdedit /set testsigning on
```

Reboot the VM. You should see “Test Mode” on the desktop after reboot.

### 2) Install the virtio-snd driver

Use either Device Manager or PnPUtil.

**Device Manager (interactive):**

1. Boot the VM.
2. Open **Device Manager**.
3. Right click the virtio-snd device → **Update Driver Software...**
4. **Browse my computer for driver software**
5. Point it to the directory containing the driver package `*.inf` files:
   - Repo layout: `drivers/windows7/virtio-snd/inf/`
   - Bundle ZIP/ISO layout: `drivers\virtio-snd\x86\` or `drivers\virtio-snd\x64\`
6. When prompted, pick the INF that matches your QEMU device configuration:
   - `aero_virtio_snd.inf` (recommended; modern `PCI\VEN_1AF4&DEV_1059&REV_01`)
   - `aero-virtio-snd-legacy.inf` (stock QEMU defaults; transitional `PCI\VEN_1AF4&DEV_1018`)
   - `aero-virtio-snd-ioport.inf` (optional legacy I/O-port transport; transitional `PCI\VEN_1AF4&DEV_1018&REV_00`)

**PnPUtil (scriptable, elevated CMD):**

```bat
pnputil -i -a X:\path\to\aero_virtio_snd.inf

REM Stock QEMU (transitional PCI\VEN_1AF4&DEV_1018):
pnputil -i -a X:\path\to\aero-virtio-snd-legacy.inf

REM Transitional (legacy I/O-port transport, REV_00):
pnputil -i -a X:\path\to\aero-virtio-snd-ioport.inf
```

Reboot if prompted.

### 3) Verify the driver is bound in Device Manager

1. In **Device Manager**, locate the installed device (after successful install it should show under **Sound, video and game controllers**).
2. Right click → **Properties**:
   - **General** should show “This device is working properly.”
   - **Details** tab → **Hardware Ids** should include the revision-gated contract-v1 HWID:
      - `PCI\VEN_1AF4&DEV_1059&REV_01`
      - (Optional) `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01` (if the device exposes the Aero subsystem ID)
        and may also include other `SUBSYS_...` / `REV_..` forms.
      - If you are installing the QEMU compatibility package (`aero-virtio-snd-legacy.inf`), the HWIDs will differ; ensure they match the INF you installed.
    - **Driver** tab → **Driver Details** should include at least:
      - `aero_virtio_snd.sys` (contract v1) **or** `virtiosnd_legacy.sys` (QEMU legacy)
      - `portcls.sys`
      - `ks.sys`

### 4) Verify a render endpoint exists

1. Open **Control Panel** → **Sound** (or run `mmsys.cpl`).
2. On the **Playback** tab, confirm a new playback device exists (render endpoint).
3. Select it → **Set Default** (optional).
4. Select it → **Properties** → **Advanced** → **Test** (or use the **Configure** wizard) to trigger playback.

If you used the `wav` audio backend, the host-side `virtio-snd-*.wav` file should be created and grow when sound plays.

### 4b) Verify a capture endpoint exists

1. Open **Control Panel** → **Sound** (or run `mmsys.cpl`).
2. On the **Recording** tab, confirm a new recording device exists (capture endpoint).
3. Select it → **Set Default** (optional).
4. Use **Sound Recorder** (or any capture-capable app) to record a short sample.

Notes:

- QEMU capture requires an audio backend that provides an input source. If none is available,
  capture may record silence, but endpoint enumeration should still work.
- The `wav` audiodev backend is convenient for validating render output, but it may not provide
  a real capture source on all QEMU builds. Use a host audio backend (PulseAudio/DirectSound/etc.)
  if you need to validate non-silent capture.

## Run the selftest audio check (Task 128)

Task 128 added a **virtio-snd** render smoke test to the Windows 7 guest selftest tool:

- `drivers/windows7/tests/guest-selftest/` → `aero-virtio-selftest.exe`

Important: `aero-virtio-selftest.exe` is a **multi-driver** selftest (virtio-blk/input/net/snd). If you run it
in a VM that only has virtio-snd attached, you may see `FAIL` for the other drivers. For this virtio-snd test
plan, focus on the `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|...` marker line.

The selftest logs to:

- stdout
- `C:\aero-virtio-selftest.log`
- `COM1` (serial)

### Option A: Run `aero-virtio-selftest.exe` (recommended)

1. Copy `aero-virtio-selftest.exe` into the guest (example):
   ```bat
   mkdir C:\AeroTests
   copy aero-virtio-selftest.exe C:\AeroTests\
   ```
2. Run it (elevated CMD recommended):
    ```bat
     REM Contract v1 device (PCI\VEN_1AF4&DEV_1059&REV_01):
     C:\AeroTests\aero-virtio-selftest.exe --test-snd
    ```
    Notes:
    - `--test-snd` (alias: `--require-snd`) makes virtio-snd **required**: missing virtio-snd is treated as a FAIL.
      (If a supported virtio-snd PCI function is detected, playback is exercised automatically even without `--test-snd`.)
    - The strict `aero_virtio_snd.inf` package expects the contract-v1 HWID (`PCI\VEN_1AF4&DEV_1059&REV_01`). Under QEMU, configure the device with
      `disable-legacy=on,x-pci-revision=0x01` so the strict INF can bind.
    - If your device enumerates as transitional, install the opt-in legacy driver package (`aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`)
      and pass `--allow-virtio-snd-transitional` so the selftest accepts it (intended for QEMU bring-up/regression).
    - When `--test-snd` is enabled, the selftest also checks for a virtio-snd capture endpoint and emits
      `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS|endpoint_present` when present.
      If the capture endpoint is missing, the capture test is reported as `SKIP|endpoint_missing` unless `--require-snd-capture` is set.
    - `--test-snd-capture` runs a capture smoke test (WASAPI, fallback to waveIn) that records for a short interval.
      This passes even on silence by default; use `--require-non-silence` to require a non-silent buffer.
    - `--test-snd-buffer-limits` runs a WASAPI buffer sizing stress test and emits:
      `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS/FAIL|...`
    - `--require-snd-capture` fails the overall selftest if the capture endpoint is missing (instead of `SKIP`).
    - If no supported virtio-snd PCI function is detected (and no capture flags are set), the tool emits
      `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP` (and `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set`).
    - Use `--disable-snd` to force `SKIP` even when capture/playback flags are present.
    - Use `--disable-snd-capture` to skip capture-only checks while still exercising playback.
 3. Review `C:\aero-virtio-selftest.log` and locate the virtio-snd marker:
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS`
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL` (playback failed; also used when the device is missing or the Topology KS interface is missing)
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|driver_not_bound` / `...|wrong_service` / `...|device_error` (driver binding not healthy)
      - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP` (playback not enabled, or skipped via `--disable-snd`; see log text / capture marker for details)

If WASAPI fails, the tool logs a line like:

`virtio-snd: WASAPI failed reason=<reason> hr=0x........`

Common `reason=` values include:

- `no_matching_endpoint` (no matching ACTIVE virtio-snd render endpoint found)
- `initialize_shared_failed` / `unsupported_stream_format`

If QEMU is using the `wav` audiodev backend, successful playback should also produce a non-empty
`.wav` file on the host.

### Option B: Guest Tools audio smoke test (audio-only sanity check)

If you only want a quick “is there any audio device + can Windows play a WAV?” check, run:

```bat
X:\verify.cmd -PlayTestSound
```

Then review:

- `C:\AeroGuestTools\report.txt`
- `C:\AeroGuestTools\report.json`

## Troubleshooting

### Code 52: Windows cannot verify the digital signature

- Ensure test signing is enabled (`bcdedit /set testsigning on`) and the guest was rebooted.
- Ensure you installed the correct x86 vs x64 driver build.
- If your packages are SHA-2 signed, ensure the Win7 SHA-2 update (commonly KB3033929) is installed.

### Code 10: device cannot start
- Confirm the device HWID Windows sees (Device Manager → Properties → Details → Hardware Ids).
- Confirm QEMU is exposing virtio-snd as expected (and you used the correct QEMU device name).
- Confirm the HWID matches the INF you installed:
  - For the contract-v1 package (`inf/aero_virtio_snd.inf`), the device must expose the revision-gated HWID
    `PCI\VEN_1AF4&DEV_1059&REV_01` and QEMU must be configured with `disable-legacy=on,x-pci-revision=0x01` (when supported).
  - For the QEMU compatibility package (`inf/aero-virtio-snd-legacy.inf`), do **not** set `disable-legacy=on` (it
    removes the transitional HWID `PCI\VEN_1AF4&DEV_1018` that the legacy INF matches).
- If your QEMU build cannot override revision IDs, the contract package will not bind; use the legacy package or
  upgrade/patch QEMU to expose `REV_01`.

### Bring-up toggles (registry)

The virtio-snd driver exposes a couple of per-device bring-up toggles that can be flipped for diagnostics.
They live under the device instance’s `Device Parameters\\Parameters` subkey:

- `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend` (`REG_DWORD`)
  - Default: `0`
  - Set to `1` to force the silent null backend. This also allows `START_DEVICE` to succeed even if virtio transport bring-up fails, which is useful when debugging QEMU/device-model issues while still exercising the PortCls/WaveRT integration.
- `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\AllowPollingOnly` (`REG_DWORD`)
  - Default: `0`
  - Set to `1` to allow polling-only mode when *no usable interrupt* (neither MSI/MSI-X nor INTx) can be wired up. (Modern virtio-pci transport packages only.)

You can find `<DeviceInstancePath>` via **Device Manager → Details → “Device instance path”**.

After changing a toggle, reboot the guest or disable/enable the device so Windows re-runs `START_DEVICE`.

Backwards compatibility note: older installs may have these values under the per-device driver key (the software key for the device/driver instance). The driver checks the per-device `Device Parameters` key first and falls back to the driver key.

Example (elevated `cmd.exe`, replace `<DeviceInstancePath>`):

```cmd
REM Force the silent Null backend:
reg add "HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters" /v ForceNullBackend /t REG_DWORD /d 1 /f

REM Allow polling-only mode when no usable interrupt can be wired up (modern virtio-pci transport packages only):
reg add "HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters" /v AllowPollingOnly /t REG_DWORD /d 1 /f

REM Verify current values:
reg query "HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters" /v ForceNullBackend
reg query "HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters" /v AllowPollingOnly
```

### Driver binds, but no playback endpoint appears in Control Panel → Sound

If the PCI device binds successfully but **no render endpoint** shows up:

- Confirm **Windows Audio** and **Windows Audio Endpoint Builder** services are running.
- Confirm the driver is installing a complete PortCls/WaveRT stack:
  - A WaveRT render miniport alone is not sufficient; Windows typically also expects the correct KS filter categories and (often) a topology miniport.
- Re-check the INF:
  - `inf/aero_virtio_snd.inf` is intentionally strict and matches only `PCI\VEN_1AF4&DEV_1059&REV_01` (an optional
    `...&SUBSYS_00191AF4&REV_01` match is present but commented out).
  - `inf/aero-virtio-snd-legacy.inf` is an opt-in QEMU compatibility package (binds the transitional virtio-snd HWID
    `PCI\VEN_1AF4&DEV_1018` and does not require `REV_01`).
  - The INF must register the correct audio/KS interfaces for render (e.g. `KSCATEGORY_AUDIO`, `KSCATEGORY_RENDER`).

If you are iterating on INF/miniport registration, remove the device from Device Manager (and delete the driver package if requested) before reinstalling so updated INF state is applied.

### QEMU device option not found

- Run `qemu-system-x86_64 -device help` and search for `virtio-sound` / `virtio-snd`.
- Some distros package QEMU without certain audio backends; if `-audiodev wav,...` fails, switch to another backend supported by your host.
