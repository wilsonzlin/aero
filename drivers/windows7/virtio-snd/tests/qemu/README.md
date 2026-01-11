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
> Capture is defined by the virtio-snd contract, and the driver contains RX plumbing, but
> it is not yet exposed via PortCls as a Windows capture endpoint.

References:

- PCI ID/HWID reference: `../../docs/pci-hwids.md`
- Optional: offline/slipstream staging so Windows binds the driver on first boot:
  - `../offline-install/README.md`

## Prerequisites

- A QEMU build new enough to expose a virtio-snd PCI device.
  - Known-good reference: QEMU **8.2.x**.
- A Windows 7 SP1 VM disk image (x86 or x64).
- The virtio-snd driver package directory for the target architecture (must contain `aero-virtio-snd.inf` + `virtiosnd.sys`):
  - Repo layout (staging): `drivers/windows7/virtio-snd/inf/`
  - Bundle ZIP/ISO layout: `drivers\virtio-snd\x86\` or `drivers\virtio-snd\x64\`
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

### Audio backend alternatives

If you cannot use the `wav` backend, replace `-audiodev wav,...` with a backend supported by your host:

- Linux (PulseAudio): `-audiodev pa,id=aerosnd0`
- Windows: `-audiodev dsound,id=aerosnd0`

## Transitional vs modern virtio-snd (PCI IDs)

The virtio specification defines two PCI device IDs for virtio-snd:

- **Modern / non-transitional**: `PCI\VEN_1AF4&DEV_1059`
- **Transitional (legacy+modern)**: `PCI\VEN_1AF4&DEV_1018`

`inf/aero-virtio-snd.inf` matches **both** IDs (and both `REV_01` and short forms) for development
convenience.

Important: the **default build** of this driver uses the **legacy virtio-pci I/O-port interface**
(see `drivers/windows7/virtio-snd/docs/README.md`), so it requires the legacy I/O-port BAR to be
present. In practice this means you should use a **transitional** device under QEMU (the default).

## Verifying HWID in Device Manager

Before installing the driver (or when troubleshooting binding), confirm the device HWID that Windows sees:

1. In the Windows 7 VM, open **Device Manager**.
2. Find the virtio-snd device (before installation it may appear as an unknown “PCI Device” under **Other devices**).
3. Right-click → **Properties** → **Details** tab.
4. In the **Property** dropdown, select **Hardware Ids**.

Expected values include at least one of:

- `PCI\VEN_1AF4&DEV_1018` (transitional; expected for the legacy I/O-port driver)
- `PCI\VEN_1AF4&DEV_1059` (modern / non-transitional; requires the modern virtio-pci driver path)

More specific forms may also appear (with `SUBSYS_...`). The Aero INF matches the revision-gated
forms and the short forms for both IDs.

The automated Win7 host harness uses **modern-only** virtio devices for strict `AERO-W7-VIRTIO`
contract v1 validation (it enables `disable-legacy=on` and forces `x-pci-revision=0x01` when
supported). That setup is not compatible with the default legacy virtio-snd driver build described
in this manual test plan.

- `drivers/windows7/tests/host-harness/Invoke-AeroVirtioWin7Tests.ps1`
- `drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py`

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
5. Point it to the directory containing `aero-virtio-snd.inf`:
   - Repo layout: `drivers/windows7/virtio-snd/inf/`
   - Bundle ZIP/ISO layout: `drivers\virtio-snd\x86\` or `drivers\virtio-snd\x64\`

**PnPUtil (scriptable, elevated CMD):**

```bat
pnputil -i -a X:\path\to\aero-virtio-snd.inf
```

Reboot if prompted.

### 3) Verify the driver is bound in Device Manager

1. In **Device Manager**, locate the installed device (after successful install it should show under **Sound, video and game controllers**).
2. Right click → **Properties**:
   - **General** should show “This device is working properly.”
   - **Details** tab → **Hardware Ids** should include either:
     - `PCI\VEN_1AF4&DEV_1018` (transitional; expected for the legacy I/O-port driver), or
     - `PCI\VEN_1AF4&DEV_1059` (modern / non-transitional),
     and may also include more-specific `SUBSYS_...` / `REV_..` forms.
   - **Driver** tab → **Driver Details** should include at least:
     - `virtiosnd.sys`
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
   C:\AeroTests\aero-virtio-selftest.exe --test-snd
   ```
3. Review `C:\aero-virtio-selftest.log` and locate the virtio-snd marker:
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS`
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL`
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|device_missing` (virtio-snd PCI function not detected)
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|topology_interface_missing` (driver bound but Topology KS interface missing)
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|flag_not_set` (you forgot `--test-snd`)
    - `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|disabled` (the test was disabled via `--disable-snd`)

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
- Confirm the HWID matches one of the patterns in `inf/aero-virtio-snd.inf` (DEV_1018/DEV_1059 with optional `REV_..`/`SUBSYS_...` qualifiers).
- For the default legacy I/O-port driver, prefer the **transitional** virtio-snd device ID (`DEV_1018`) and do **not** use `disable-legacy=on`.

### Driver binds, but no playback endpoint appears in Control Panel → Sound

If the PCI device binds successfully but **no render endpoint** shows up:

- Confirm **Windows Audio** and **Windows Audio Endpoint Builder** services are running.
- Confirm the driver is installing a complete PortCls/WaveRT stack:
  - A WaveRT render miniport alone is not sufficient; Windows typically also expects the correct KS filter categories and (often) a topology miniport.
- Re-check the INF:
  - The INF matches both `DEV_1018` (transitional) and `DEV_1059` (modern) forms, with and without `REV_..` / `SUBSYS_...` qualifiers.
  - The INF must register the correct audio/KS interfaces for render (e.g. `KSCATEGORY_AUDIO`, `KSCATEGORY_RENDER`).

If you are iterating on INF/miniport registration, remove the device from Device Manager (and delete the driver package if requested) before reinstalling so updated INF state is applied.

### QEMU device option not found

- Run `qemu-system-x86_64 -device help` and search for `virtio-sound` / `virtio-snd`.
- Some distros package QEMU without certain audio backends; if `-audiodev wav,...` fails, switch to another backend supported by your host.
