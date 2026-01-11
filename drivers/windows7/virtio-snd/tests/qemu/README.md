<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# QEMU manual test plan: Windows 7 virtio-snd (PortCls/WaveRT) driver

This document describes a repeatable way to manually validate a Windows 7 **virtio-snd** audio driver end-to-end under **QEMU**.

What this test plan verifies:

1. QEMU exposes a virtio-snd PCI function with a stable hardware ID (HWID)
2. Windows 7 binds the virtio-snd driver package (INF/SYS) to that PCI function
3. The Windows audio stack enumerates a **render** endpoint (Control Panel → Sound)
4. Audio playback works (audible on the host, or captured to a WAV file in headless runs)
5. (Future) The Windows audio stack enumerates a **capture** endpoint (Control Panel → Sound → Recording)
6. (Future) Audio capture works (records host input if available, otherwise records silence)
7. The **virtio-snd** portion of the guest audio smoke test passes (selftest **Task 128**)

> Note: The current Aero Windows 7 virtio-snd driver package is **render-only** (playback).
> Capture is defined by `AERO-W7-VIRTIO` v1 (`rxq`, stream id `1`), but the current
> PortCls driver is render-only and does not expose a Windows capture endpoint yet.

References:

- PCI ID/HWID reference: `../../docs/pci-hwids.md`
- Optional: offline/slipstream staging so Windows binds the driver on first boot:
  - `../offline-install/README.md`

## Prerequisites

- A QEMU build new enough to expose a virtio-snd PCI device.
  - Known-good reference: QEMU **8.2.x**.
  - For binding with the strict Aero contract v1 INF (`aero-virtio-snd.inf`), QEMU must support
    `disable-legacy=on` and `x-pci-revision=0x01` on the virtio-snd device (to match `DEV_1059&REV_01`).
  - Verify supported properties with `qemu-system-x86_64 -device virtio-sound-pci,help`.
- A Windows 7 SP1 VM disk image (x86 or x64).
- A virtio-snd driver package staged next to the device INF:
  - Repo layout (staging): `drivers/windows7/virtio-snd/inf/`
  - Bundle ZIP/ISO layout: `drivers\\virtio-snd\\x86\\` or `drivers\\virtio-snd\\x64\\`
  - **Recommended for stock QEMU defaults** (transitional `DEV_1018`, typically `REV_00`):
    - `inf/aero-virtio-snd-legacy.inf`
    - `virtiosnd_legacy.sys` built for x86 or x64 (MSBuild `Configuration=Legacy`)
  - **Optional: strict Aero contract v1** (modern `DEV_1059&REV_01`):
    - `inf/aero-virtio-snd.inf`
    - `virtiosnd.sys` built for x86 or x64
- Test signing enabled in the guest (or a properly production-signed driver package).

Optional (but recommended for headless hosts):

- A QEMU audio backend that does not require host audio hardware.
  - Recommended: `-audiodev wav,...` (captures guest audio to a host `.wav` file).

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
  -device virtio-sound-pci,audiodev=aerosnd0
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
  -device virtio-sound-pci,audiodev=aerosnd0
```

These examples use stock QEMU defaults (transitional `DEV_1018`); install `aero-virtio-snd-legacy.inf`.
To validate the strict Aero contract v1 identity under QEMU, append `,disable-legacy=on,x-pci-revision=0x01`
to the virtio-snd device (if supported) and install `aero-virtio-snd.inf`.

### Audio backend alternatives

If you cannot use the `wav` backend, replace `-audiodev wav,...` with a backend supported by your host:

- Linux (PulseAudio): `-audiodev pa,id=aerosnd0`
- Windows: `-audiodev dsound,id=aerosnd0`

## Supported PCI IDs (Aero contract vs QEMU)

The shipped Aero Windows 7 virtio-snd driver package targets `AERO-W7-VIRTIO` v1,
which is **virtio-pci modern only** (modern ID space + `REV_01`). For stock QEMU defaults
(transitional `DEV_1018`, typically `REV_00`), use the opt-in legacy package.

- **Modern / non-transitional (Aero contract v1):** `PCI\VEN_1AF4&DEV_1059` (requires `REV_01`)
- **Transitional (QEMU default):** `PCI\VEN_1AF4&DEV_1018`

### Which INF to install

- **Stock QEMU (transitional `DEV_1018`, usually `REV_00`):**
  - Install `inf/aero-virtio-snd-legacy.inf` → `virtiosnd_legacy.sys`
- **Strict Aero contract v1 (modern `DEV_1059` + `REV_01`):**
  - Install `inf/aero-virtio-snd.inf` → `virtiosnd.sys`

### QEMU properties (optional)

- To keep QEMU in transitional mode (for the legacy INF), **do not** set `disable-legacy=on`.
- To validate Aero contract v1 under QEMU, you typically need:
  - `disable-legacy=on` (so QEMU reports `DEV_1059`), and
  - `x-pci-revision=0x01` (so QEMU reports `REV_01`, if supported by your build).

Tip: confirm your QEMU build supports these properties with (replace `virtio-sound-pci` with the device name from
above if needed):

```bash
qemu-system-x86_64 -device virtio-sound-pci,help
```

Development note: the repo also contains an optional legacy filename alias INF
(`inf/virtio-snd.inf.disabled`). If you rename it back to `virtio-snd.inf`, it installs the same
driver/service as `aero-virtio-snd.inf` (same contract-v1 HWIDs), but provides the legacy filename
for compatibility with older tooling/workflows and uses `CatalogFile = virtio-snd.cat`. This legacy
INF is **not** staged into the CI driver bundle.

## Verifying HWID in Device Manager

Before installing the driver (or when troubleshooting binding), confirm the device HWID that Windows sees:

1. In the Windows 7 VM, open **Device Manager**.
2. Find the virtio-snd device (before installation it may appear as an unknown “PCI Device” under **Other devices**).
3. Right-click → **Properties** → **Details** tab.
4. In the **Property** dropdown, select **Hardware Ids**.

Expected values include:

- `PCI\VEN_1AF4&DEV_1018` (transitional; expected for stock QEMU defaults)
- `PCI\VEN_1AF4&DEV_1059&REV_01` (Aero contract v1; expected if QEMU is configured as modern-only + `REV_01`)

More specific forms may also appear (with `SUBSYS_...` / `REV_...`). Ensure the HWID matches the
INF you intend to install (`aero-virtio-snd.inf` vs `aero-virtio-snd-legacy.inf`).

If you see `PCI\VEN_1AF4&DEV_1018` (transitional; stock QEMU default), install the legacy package
(`aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`). If you want to validate the strict Aero contract v1
package, configure QEMU to expose `DEV_1059&REV_01` (for example `disable-legacy=on,x-pci-revision=0x01`
when supported).

## Preferred: automated host harness

For repeatable automated validation (including strict `AERO-W7-VIRTIO` v1 device configuration),
prefer the Windows 7 host harness. It probes QEMU for supported virtio-snd device properties (via
`-device virtio-sound-pci,help`) and enables contract-v1 identification when supported (for example
`disable-legacy=on` and `x-pci-revision=0x01`).

- `drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1`
- `drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py`

The harness configures **modern-only** virtio devices (it enables `disable-legacy=on` and forces
`x-pci-revision=0x01` when supported), which matches the default virtio-snd driver + INF contract.

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
   - For stock QEMU, pick `aero-virtio-snd-legacy.inf` when prompted.

**PnPUtil (scriptable, elevated CMD):**

```bat
pnputil -i -a X:\path\to\aero-virtio-snd-legacy.inf
```

Reboot if prompted.

### 3) Verify the driver is bound in Device Manager

1. In **Device Manager**, locate the installed device (after successful install it should show under **Sound, video and game controllers**).
2. Right click → **Properties**:
   - **General** should show “This device is working properly.”
   - **Details** tab → **Hardware Ids** should include either:
     - `PCI\VEN_1AF4&DEV_1018` (transitional; stock QEMU), or
     - `PCI\VEN_1AF4&DEV_1059&REV_01` (Aero contract v1),
     and may also include more-specific `SUBSYS_...` / `REV_..` forms.
   - **Driver** tab → **Driver Details** should include at least:
     - `virtiosnd.sys` (contract v1) **or** `virtiosnd_legacy.sys` (QEMU legacy)
      - `portcls.sys`
      - `ks.sys`

### 4) Verify a render endpoint exists

1. Open **Control Panel** → **Sound** (or run `mmsys.cpl`).
2. On the **Playback** tab, confirm a new playback device exists (render endpoint).
3. Select it → **Set Default** (optional).
4. Select it → **Properties** → **Advanced** → **Test** (or use the **Configure** wizard) to trigger playback.

If you used the `wav` audio backend, the host-side `virtio-snd-*.wav` file should be created and grow when sound plays.

### 4b) (Future) Verify a capture endpoint exists

This section is forward-looking. It will not pass until a PortCls capture endpoint
is implemented for virtio-snd stream id `1` (RX).

1. Open **Control Panel** → **Sound** (or run `mmsys.cpl`).
2. On the **Recording** tab, confirm a new recording device exists (capture endpoint).
3. Use **Sound Recorder** (or any capture-capable app) to record a short sample.

Notes:

- QEMU capture requires an audio backend that provides an input source. If none is available,
  capture may record silence, but endpoint enumeration should still work.

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
   REM Contract v1 device (DEV_1059):
   C:\AeroTests\aero-virtio-selftest.exe --test-snd
 
    REM Stock QEMU transitional device (DEV_1018) + legacy INF/package:
    C:\AeroTests\aero-virtio-selftest.exe --test-snd --allow-virtio-snd-transitional
    ```
    Notes:
    - `--test-snd` (alias: `--require-snd`) enables virtio-snd playback testing. Missing virtio-snd is treated as a FAIL in this mode.
    - If your device enumerates as transitional (`PCI\VEN_1AF4&DEV_1018`), pass `--allow-virtio-snd-transitional`
      so the selftest accepts the transitional ID (intended for QEMU bring-up/regression).
    - If you run without `--test-snd` / `--require-snd`, the tool emits `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP`
      (and `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set`).
    - Use `--disable-snd` to force `SKIP` even when capture/playback flags are present.
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
- Confirm the HWID matches one of the patterns in the INF you installed:
  - `inf/aero-virtio-snd-legacy.inf`: expects `DEV_1018` (no `REV_01` requirement)
  - `inf/aero-virtio-snd.inf`: expects `DEV_1059&REV_01`
- If you installed the legacy package, do **not** use `disable-legacy=on` (it removes the transitional ID).
- If you installed the contract package, QEMU must expose `DEV_1059&REV_01` (for example:
  `-device virtio-sound-pci,...,disable-legacy=on,x-pci-revision=0x01` when supported).
- If your QEMU build cannot override revision IDs, the contract package will not bind; use the legacy package or upgrade/patch QEMU to expose `REV_01`.

### Driver binds, but no playback endpoint appears in Control Panel → Sound

If the PCI device binds successfully but **no render endpoint** shows up:

- Confirm **Windows Audio** and **Windows Audio Endpoint Builder** services are running.
- Confirm the driver is installing a complete PortCls/WaveRT stack:
  - A WaveRT render miniport alone is not sufficient; Windows typically also expects the correct KS filter categories and (often) a topology miniport.
- Re-check the INF:
  - `aero-virtio-snd.inf` is strict and matches only `DEV_1059&REV_01` (an optional `...&SUBSYS_00191AF4&REV_01` match is present but commented out).
  - `aero-virtio-snd-legacy.inf` matches the transitional ID `DEV_1018` (no `REV_01` requirement).
  - The INF must register the correct audio/KS interfaces for render (e.g. `KSCATEGORY_AUDIO`, `KSCATEGORY_RENDER`).

If you are iterating on INF/miniport registration, remove the device from Device Manager (and delete the driver package if requested) before reinstalling so updated INF state is applied.

### QEMU device option not found

- Run `qemu-system-x86_64 -device help` and search for `virtio-sound` / `virtio-snd`.
- Some distros package QEMU without certain audio backends; if `-audiodev wav,...` fails, switch to another backend supported by your host.
