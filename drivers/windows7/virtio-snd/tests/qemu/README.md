# QEMU manual test plan: Windows 7 virtio-snd (PortCls/WaveRT) driver

This document describes a repeatable way to manually validate a Windows 7 **virtio-snd** audio driver end-to-end under **QEMU**.

What this test plan verifies:

1. QEMU exposes a virtio-snd PCI function with a stable hardware ID (HWID)
2. Windows 7 binds the virtio-snd driver package (INF/SYS) to that PCI function
3. The Windows audio stack enumerates a **render** endpoint (Control Panel → Sound)
4. Audio playback works (audible on the host, or captured to a WAV file in headless runs)
5. The guest audio smoke test passes (selftest **Task 128**)

References:

- PCI ID/HWID reference: `drivers/windows7/virtio-snd/docs/pci-hwids.md`
- Optional: offline/slipstream staging so Windows binds the driver on first boot:
  - `drivers/windows7/virtio-snd/tests/offline-install/README.md`

## Prerequisites

- A QEMU build new enough to expose a virtio-snd PCI device.
  - Known-good reference: QEMU **8.2.x**.
- A Windows 7 SP1 VM disk image (x86 or x64).
- The virtio-snd driver package for the target architecture:
  - `drivers/windows7/virtio-snd/inf/virtio-snd.inf`
  - `virtiosnd.sys` built for x86 or x64 and placed next to the INF for installation
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
  -device virtio-sound-pci,disable-legacy=on,audiodev=aerosnd0
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
  -device virtio-sound-pci,disable-legacy=on,audiodev=aerosnd0
```

### Audio backend alternatives

If you cannot use the `wav` backend, replace `-audiodev wav,...` with a backend supported by your host:

- Linux (PulseAudio): `-audiodev pa,id=aerosnd0`
- Windows: `-audiodev dsound,id=aerosnd0`

## Modern-only vs transitional virtio-snd (PCI IDs)

Virtio-snd has two PCI IDs defined in the virtio spec:

- **Modern / non-transitional**: `PCI\VEN_1AF4&DEV_1059` (**Aero contract v1**)
- **Transitional (legacy+modern)**: `PCI\VEN_1AF4&DEV_1018`

Aero contract v1 is **modern-only**, so this test plan expects `DEV_1059`.

To make the device enumerate as modern/non-transitional under QEMU, include `disable-legacy=on` (as shown in the command lines above).

## Verifying HWID in Device Manager

Before installing the driver (or when troubleshooting binding), confirm the device HWID that Windows sees:

1. In the Windows 7 VM, open **Device Manager**.
2. Find the virtio-snd device (before installation it may appear as an unknown “PCI Device” under **Other devices**).
3. Right-click → **Properties** → **Details** tab.
4. In the **Property** dropdown, select **Hardware Ids**.

Expected values include at least one of:

- `PCI\VEN_1AF4&DEV_1059` (modern / non-transitional; expected for contract v1)
- `PCI\VEN_1AF4&DEV_1018` (transitional; if you did not use `disable-legacy=on`)

More specific forms may also appear (with `SUBSYS_...` and `REV_...`). The INF should match the shorter `VEN/DEV` form.

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
5. Point it to the directory containing `virtio-snd.inf`:
   - Repo layout: `drivers/windows7/virtio-snd/inf/`
   - Bundle ZIP/ISO layout: `drivers\virtio-snd\x86\` or `drivers\virtio-snd\x64\`

**PnPUtil (scriptable, elevated CMD):**

```bat
pnputil -i -a X:\path\to\virtio-snd.inf
```

Reboot if prompted.

### 3) Verify the driver is bound in Device Manager

1. In **Device Manager**, locate the installed device (after successful install it should show under **Sound, video and game controllers**).
2. Right click → **Properties**:
   - **General** should show “This device is working properly.”
   - **Details** tab → **Hardware Ids** should include `PCI\VEN_1AF4&DEV_1059...`
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

## Run the selftest audio check (Task 128)

The Windows 7 Guest Tools verification script includes an audio smoke test that can also attempt to play a system `.wav`.

1. In the guest, open an elevated Command Prompt.
2. Run (from the Guest Tools media):
   ```bat
   X:\verify.cmd -PlayTestSound
   ```
3. Review the logs:
   - `C:\AeroGuestTools\report.txt`
   - `C:\AeroGuestTools\report.json`

How to interpret results:

- `Smoke Test: Audio` reports `PASS/WARN/FAIL` based on:
  - whether a `Win32_SoundDevice` is present, and
  - whether `-PlayTestSound` succeeded.
- If QEMU is using the `wav` audiodev backend, successful playback should also produce a non-empty `.wav` file on the host.

## Troubleshooting

### Code 52: Windows cannot verify the digital signature

- Ensure test signing is enabled (`bcdedit /set testsigning on`) and the guest was rebooted.
- Ensure you installed the correct x86 vs x64 driver build.
- If your packages are SHA-2 signed, ensure the Win7 SHA-2 update (commonly KB3033929) is installed.

### Code 10: device cannot start

- Confirm the device HWID Windows sees (Device Manager → Properties → Details → Hardware Ids).
- Confirm QEMU is exposing virtio-snd as expected (and you used the correct QEMU device name).
- If you forced `disable-legacy=on`, try without it (or vice versa) to confirm whether the failure is specific to modern vs transitional mode.

### Driver binds, but no playback endpoint appears in Control Panel → Sound

If the PCI device binds successfully but **no render endpoint** shows up:

- Confirm **Windows Audio** and **Windows Audio Endpoint Builder** services are running.
- Confirm the driver is installing a complete PortCls/WaveRT stack:
  - A WaveRT render miniport alone is not sufficient; Windows typically also expects the correct KS filter categories and (often) a topology miniport.
- Re-check the INF:
  - The INF must match the emitted PCI ID (`DEV_1059` vs `DEV_1018`).

If you are iterating on INF/miniport registration, remove the device from Device Manager (and delete the driver package if requested) before reinstalling so updated INF state is applied.

### QEMU device option not found

- Run `qemu-system-x86_64 -device help` and search for `virtio-sound` / `virtio-snd`.
- Some distros package QEMU without certain audio backends; if `-audiodev wav,...` fails, switch to another backend supported by your host.
