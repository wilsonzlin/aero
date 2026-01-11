# QEMU manual test plan: Windows 7 virtio-snd (PortCls/WaveRT) driver

This document describes a repeatable way to manually validate a Windows 7 **virtio-snd** audio driver end-to-end under **QEMU**.

What this test plan verifies:

1. QEMU exposes a virtio-snd PCI function with a stable hardware ID (HWID)
2. Windows 7 binds the virtio-snd driver package (INF/SYS) to that PCI function
3. The Windows audio stack enumerates a **render** endpoint (Control Panel → Sound)
4. Audio playback works (audible on the host, or captured to a WAV file in headless runs)

## Prerequisites

- A QEMU build new enough to expose a virtio-snd PCI device.
- A Windows 7 SP1 VM disk image (x86 or x64).
- The virtio-snd driver package for the target architecture:
  - `drivers/windows7/virtio-snd/inf/virtio-snd.inf`
  - `virtiosnd.sys` built for x86 or x64 and placed next to the INF for installation
- Test signing enabled in the guest (or a properly production-signed driver package).

If you want Windows to bind the driver automatically on first boot (no manual “Have Disk…” step), see:

- `../offline-install/README.md` — offline/slipstream staging with DISM (`install.wim`, offline `\Windows\`, and optional `boot.wim`)

Optional (but recommended for headless hosts):

- A QEMU audio backend that works without host audio hardware, e.g. `-audiodev wav,...` or `-audiodev none,...`.

## Recommended QEMU versions

Virtio-snd is relatively new compared to virtio-blk/virtio-net. For the most predictable results, use a recent QEMU release (QEMU **7.x+** recommended).

If your QEMU build does not list a virtio-snd device in `-device help`, upgrade QEMU.

## Identify the virtio-snd device name in your QEMU build

Virtio-snd is relatively new in QEMU, and device naming can vary by version/build. Always confirm what your QEMU binary calls the device:

```bash
qemu-system-x86_64 -device help | grep -E "virtio-(sound|snd)"
```

Common outputs include:

- `virtio-sound-pci` (typical in modern QEMU)
- `virtio-snd-pci` (some builds/aliases)

If neither appears, upgrade QEMU.

## QEMU command lines

The examples below are intentionally explicit and can be used as a starting point. Adjust paths, CPU accel, and disk/network options as needed.

### Audio backend options (headless-friendly)

QEMU audio devices should be paired with an explicit `-audiodev` so runs are deterministic:

- **Capture audio to a file** (recommended for headless validation):
  - `-audiodev wav,id=aerosnd0,path=virtio-snd.wav`
- **Discard audio** (device still enumerates, but you will not hear output):
  - `-audiodev none,id=aerosnd0`

In both cases, attach virtio-snd with an explicit `audiodev=`:

```
-device virtio-sound-pci,audiodev=aerosnd0
```

(Replace `virtio-sound-pci` with the device name discovered via `-device help`.)

### Windows 7 SP1 x86

```bash
qemu-system-i386 \
  -machine pc,accel=kvm \
  -m 2048 \
  -cpu qemu32 \
  -drive file=win7-x86.qcow2,if=virtio,format=qcow2 \
  -netdev user,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -audiodev wav,id=aerosnd0,path=virtio-snd-x86.wav \
  -device virtio-sound-pci,audiodev=aerosnd0
```

To run without networking, drop the `-netdev ...` and `-device virtio-net-pci,...` lines.

### Windows 7 SP1 x64

```bash
qemu-system-x86_64 \
  -machine pc,accel=kvm \
  -m 4096 \
  -cpu qemu64 \
  -drive file=win7-x64.qcow2,if=virtio,format=qcow2 \
  -netdev user,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -audiodev wav,id=aerosnd0,path=virtio-snd-x64.wav \
  -device virtio-sound-pci,audiodev=aerosnd0
```

### Modern-only vs transitional virtio-snd (PCI IDs)

Virtio-snd has two PCI IDs defined in the virtio spec:

- **Modern / non-transitional**: `PCI\VEN_1AF4&DEV_1059`
- **Transitional (legacy+modern)**: `PCI\VEN_1AF4&DEV_1018`

`drivers/windows7/virtio-snd/inf/virtio-snd.inf` is expected to match both.

If you want to make your intent explicit (and force the modern/non-transitional PCI ID), include:

```bash
-device virtio-sound-pci,disable-legacy=on,audiodev=aerosnd0
```

## Guest-side validation (Windows 7)

### 1) Enable test signing (if needed)

On Windows 7 x64, kernel drivers must be signed unless test signing / signature enforcement overrides are enabled.

From an elevated Command Prompt:

```bat
bcdedit /set testsigning on
```

Reboot the VM. You should see “Test Mode” on the desktop after reboot.

### 2) Install the virtio-snd driver

1. Boot the VM.
2. Open **Device Manager**.
3. Find the virtio-snd device (before installation it may appear as an unknown “PCI Device” under **Other devices**).
4. Right click → **Update Driver Software...**
5. **Browse my computer for driver software**
6. Point it to: `drivers/windows7/virtio-snd/inf/`
7. Reboot if prompted.

### 3) Verify the driver is bound in Device Manager

1. In **Device Manager**, locate the installed device:
   - Typical category: **Sound, video and game controllers** (after successful install)
2. Right click → **Properties**:
   - **General** should show “This device is working properly.”
   - **Details** tab → **Hardware Ids** should include one of:
     - `PCI\VEN_1AF4&DEV_1059`
     - `PCI\VEN_1AF4&DEV_1018`
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

### 5) (Optional) Future selftest integration

Once the guest selftest includes a virtio-snd test (see `drivers/windows7/tests/`), use:

- `aero-virtio-selftest.exe` → virtio-snd test

instead of manually triggering playback in the UI.

## Troubleshooting

### Code 52: Windows cannot verify the digital signature

- Ensure test signing is enabled (`bcdedit /set testsigning on`) and the guest was rebooted.
- Ensure you installed the correct x86 vs x64 driver build.
- If your driver package is completely unsigned, Windows 7 x64 may still refuse to load it even in test mode; prefer test-signing the SYS with a development certificate and importing that certificate into the guest.

### Code 10: device cannot start

- Confirm the device HWID Windows sees (Device Manager → Properties → Details → Hardware Ids).
- Confirm QEMU is exposing virtio-snd as expected (and you used the correct QEMU device name).
- If you forced `disable-legacy=on`, try without it (or vice versa) to see if the driver expects transitional vs modern behavior.

### Driver binds, but no playback endpoint appears in Control Panel → Sound

If the PCI device binds successfully but **no render endpoint** shows up:

- Confirm **Windows Audio** and **Windows Audio Endpoint Builder** services are running.
- Confirm the driver is installing a complete PortCls/WaveRT stack:
  - A WaveRT render miniport alone is not sufficient; Windows expects the correct KS filter categories and (typically) a topology miniport as well.
- Re-check the INF:
  - The INF must match the emitted PCI ID (`DEV_1059` vs `DEV_1018`).
  - The INF must register the correct audio/KS interfaces for render (e.g. `KSCATEGORY_AUDIO`, `KSCATEGORY_RENDER`).

If you are iterating on INF/miniport registration, remove the device from Device Manager (check “Delete the driver software for this device” if present) before reinstalling to ensure the new registry/INF state is applied.
