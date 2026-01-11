# Windows 7 Driver Troubleshooting (Aero Guest Tools)

This document covers common Windows 7 issues after installing **Aero Guest Tools** and switching the VM from baseline emulated devices (**AHCI/e1000/VGA**) to paravirtual devices (**virtio + Aero GPU**).

If you have not installed Guest Tools yet, start here:

- [`docs/windows7-guest-tools.md`](./windows7-guest-tools.md)

## Before you start: quick triage checklist

1. **Don’t keep rebooting** if you hit a boot loop or `0x7B` BSOD after switching storage. Power off and use the rollback path.
2. Collect `report.txt` by running `verify.cmd` as Administrator and opening `C:\AeroGuestTools\report.txt`. Pay special attention to any `Code 52` (signing/trust) or `Code 28` (driver not installed) device errors.
3. Confirm you’re using drivers that match your OS:
   - Windows 7 **x86** requires x86 drivers.
   - Windows 7 **x64** requires x64 drivers. (32-bit drivers cannot load.)
4. If you changed multiple VM devices at once (storage + GPU + network), consider rolling back and switching **one class at a time** so failures are easier to isolate.
5. Confirm the guest **date/time** is correct. If the clock is far off, Windows may treat certificates as “not yet valid” or “expired” and driver signature validation can fail.

## Quick links by symptom

- Driver signature / trust failures:
  - [Device Manager Code 52 (signature and trust failures)](#issue-device-manager-code-52-signature-and-trust-failures)
  - [Catalog hash mismatch (hash not present in specified catalog file)](#issue-catalog-hash-mismatch-hash-not-present-in-specified-catalog-file)
  - [Guest Tools media integrity check fails (manifest hash mismatch)](#issue-guest-tools-media-integrity-check-fails-manifest-hash-mismatch)
  - [Missing KB3033929 (SHA-256 signature support)](#issue-missing-kb3033929-sha-256-signature-support)
- Driver installed but not working:
  - [Device Manager Code 28 (drivers not installed)](#issue-device-manager-code-28-drivers-not-installed)
  - [Device Manager Code 10 (device cannot start)](#issue-device-manager-code-10-device-cannot-start)
  - [Virtio device not found or Unknown device after switching](#issue-virtio-device-not-found-or-unknown-device-after-switching)
  - [Lost keyboard/mouse after switching to virtio-input](#issue-lost-keyboardmouse-after-switching-to-virtio-input)
- Boot failures after switching storage:
  - [Storage controller switch gotchas (boot loops, 0x7B)](#issue-storage-controller-switch-gotchas-boot-loops-0x7b)
  - [No bootable device or BOOTMGR is missing after switching storage](#issue-no-bootable-device-or-bootmgr-is-missing-after-switching-storage)
- Windows Setup disk detection issues:
  - [Windows Setup can't see a virtio-blk disk](#issue-windows-setup-cant-see-a-virtio-blk-disk-slipstream-installs)
- Display issues after switching to the Aero GPU:
  - [Black screen after switching to the Aero GPU](#issue-black-screen-after-switching-to-the-aero-gpu)
  - [Aero theme not available (stuck in basic graphics mode)](#issue-aero-theme-not-available-stuck-in-basic-graphics-mode)
- Guest Tools installation problems:
  - [`setup.cmd` fails (won't run)](#issue-setupcmd-fails-wont-run)
  - [Safe Mode recovery tips](#safe-mode-recovery-tips)
- Expected behavior:
  - [Test Mode watermark on the desktop (x64)](#issue-test-mode-watermark-on-the-desktop-x64)
- Diagnostics:
  - [Collecting useful logs](#collecting-useful-logs)
  - [Finding device Hardware IDs](#finding-device-hardware-ids)
  - [Capturing BSOD stop codes](#capturing-bsod-stop-codes)

## Collecting useful logs

If you need to debug driver install failures, these are the most useful artifacts to gather:

- `C:\AeroGuestTools\report.txt` (from `verify.cmd`)
- `C:\AeroGuestTools\report.json` (from `verify.cmd`, machine-readable)
- `C:\AeroGuestTools\install.log` (from `setup.cmd`)
- `C:\AeroGuestTools\uninstall.log` (from `uninstall.cmd`, if used)
- Device Manager → device → **Properties**:
  - **General** tab (error code)
  - **Events** tab (device install/start failures)
- Driver installation log:
  - `C:\Windows\inf\setupapi.dev.log`
    - Tip: open it and search for the device’s Hardware ID or the `.inf` name.

## Finding device Hardware IDs

If you are manually binding a driver (or filing a bug), the **Hardware IDs** are the most useful identifier.

1. Open **Device Manager**.
2. Right-click the device → **Properties**.
3. Open the **Details** tab.
4. Select **Hardware Ids**.

You can use these IDs to:

- confirm the VM is presenting the device you think it is,
- verify you are installing the correct driver package (especially x86 vs x64 and device class),
- search `setupapi.dev.log` to see why a driver did (or didn’t) bind.

## Capturing BSOD stop codes

If Windows blue-screens and immediately reboots, you lose the most important clue (the stop code).

To force the BSOD to stay on screen:

1. Reboot.
2. Press **F8** before the Windows logo appears.
3. Select **Disable automatic restart on system failure**.
4. Reboot again and reproduce the failure; note the stop code (for example `0x0000007B`).

## Safe rollback path (storage boot failure)

If Windows fails to boot after switching the system disk from **AHCI → virtio-blk**:

1. Power off the VM.
2. Switch the disk controller back to **AHCI**.
3. Boot Windows (it should boot again).
4. Re-run `setup.cmd` as Administrator and reboot once on AHCI.
5. Try switching to virtio-blk again.

Why this works: Windows can only boot from a storage controller if its driver is installed and configured as boot-critical. Going back to AHCI restores the known-good boot path so you can fix the driver configuration from inside Windows.

Tip: in `report.txt`, check:

- **virtio-blk Storage Service**: should show the configured storage service with `Start=0 (BOOT_START)`
- **virtio-blk Boot Critical Registry**: should show no missing/mismatched `CriticalDeviceDatabase` keys

## Issue: Device Manager Code 52 (signature and trust failures)

**Symptom**

- Device Manager shows a yellow warning icon and:
  - `Windows cannot verify the digital signature for the drivers required for this device. (Code 52)`

**Common causes**

- **Windows 7 x64** is not in **Test Mode** but the drivers are test-signed.
- The Aero driver signing certificate was not installed into the correct certificate stores.
- Windows 7 is missing **KB3033929**, so it cannot validate **SHA-256** signatures.
- You installed the wrong-architecture driver package (x86 vs x64).
  - Windows 7 **x86**: drivers can install with warnings, but you can still end up with Code 52 if the package is malformed or not trusted as expected.
- The guest clock is incorrect, so certificate validity checks fail.

### Fix steps

1. **Confirm test signing state (x64):**
   - Open an elevated Command Prompt and run:
     - `bcdedit /enum {current}`
   - Look for `testsigning Yes`.
   - If needed, enable it:
     - `bcdedit /set {current} testsigning on`
     - Reboot.

2. **Confirm Aero certificate is installed (recommended for both x86 and x64):**
   - Run `certlm.msc` (Local Computer certificate manager).
   - Check:
     - **Trusted Root Certification Authorities → Certificates**
     - **Trusted Publishers → Certificates**
   - If the certificate is missing, re-run `setup.cmd` as Administrator.

3. **Check KB3033929 (SHA-256 support):**
   - See the KB3033929 section below.

4. **Reinstall the driver:**
   - Re-run `setup.cmd` as Administrator.
   - Or in Device Manager:
     - Right-click the device → **Update Driver Software…**
     - Choose **Browse my computer for driver software**
     - Browse to your Guest Tools driver folder

5. **Confirm the driver package is staged in the driver store (optional but useful):**
   - In an elevated Command Prompt:
     - `pnputil -e`
   - Look for the published name (`oemXX.inf`) associated with the Aero/virtio devices.
   - If you re-run `setup.cmd`, it should stage any missing packages automatically.

### One-time bypass (not recommended as the primary path)

On Windows 7 x64 you can sometimes boot once with driver signature enforcement disabled:

1. Reboot.
2. Press **F8** before Windows starts.
3. Select **Disable Driver Signature Enforcement**.

This only affects that one boot. For a repeatable setup, prefer installing properly signed/test-signed drivers and configuring test signing as required.

## Issue: Catalog hash mismatch (hash not present in specified catalog file)

**Symptom**

During driver installation (or on boot), Windows reports an error like:

- `The hash for the file is not present in the specified catalog file. The file is likely corrupt or the victim of tampering.`

**Common causes**

- The Guest Tools media is corrupted or incomplete.
- The `.cat` file does not match the `.sys`/`.inf` (wrong driver set or mixed versions).
- Signature validation is failing (for example: incorrect system time, missing KB3033929 for SHA-256).

**Fix**

1. Verify the guest clock/date/time is correct.
2. Ensure KB3033929 is installed if your drivers are SHA-256-signed.
3. Replace the Guest Tools ISO with a fresh copy (don’t mix driver folders across versions).
4. Re-run `setup.cmd` as Administrator (or use the manual install fallback).

## Issue: Guest Tools media integrity check fails (manifest hash mismatch)

This issue is specific to the Guest Tools diagnostics output (it’s detected by `verify.cmd`), but it usually causes driver install failures that look like catalog/signature problems.

**Symptom**

- `verify.cmd` reports **FAIL** in **Guest Tools Media Integrity (manifest.json)** and lists:
  - missing files, and/or
  - SHA-256 hash mismatches for files on the Guest Tools media.

**Common causes**

- The ISO/zip was corrupted in transit.
- Only part of the ISO contents were copied/extracted.
- Files from two different Guest Tools releases were mixed together (for example, overwriting `drivers\` but keeping an older `manifest.json`).

**Fix**

1. Replace the Guest Tools ISO/zip with a fresh copy.
2. Ensure you copy/extract **the entire media root** (including `drivers\`, `certs\`, `config\`).
3. Re-run `setup.cmd` as Administrator after replacing the media.

## Issue: Missing KB3033929 (SHA-256 signature support)

Windows 7 needs KB3033929 to validate many SHA-256 signatures. Without it, drivers that are correctly signed may still appear “unsigned”.

**How to check if it’s installed**

- Control Panel → Programs and Features → View installed updates → search for `KB3033929`
- Or in an elevated Command Prompt:
  - `wmic qfe | find "3033929"`

**How to fix**

1. Download the correct KB3033929 `.msu` for your architecture on a host machine:
   - Windows 7 x86 → x86 update
   - Windows 7 x64 → x64 update
2. Copy the `.msu` into the VM (ISO, network, or shared folder).
3. Run the `.msu` inside the VM and reboot.

### Related SHA-2 updates (sometimes required)

Depending on how your driver packages and certificates are signed, stock Windows 7 SP1 may also require additional SHA-2 updates (for example **KB4474419**).

You can check for these updates with:

- `wmic qfe | find "4474419"`
- `wmic qfe | find "4490628"` (common servicing stack prerequisite for installing newer updates)

If KB3033929/KB4474419 fails to install, ensure you are on Windows 7 SP1 and install the required servicing stack updates first.

### Recommended signing algorithm policy (for compatibility)

If you are producing or selecting driver packages for Windows 7:

- **Best out-of-box compatibility:** SHA-1-signed catalogs (works on a fresh SP1 install).
- **SHA-256-only signing:** requires KB3033929 (and users frequently don’t have it offline).
- **Practical approach:** provide a path that works both ways:
  - Ensure Guest Tools clearly tells users when KB3033929 is required, and/or
  - Provide a SHA-1-signed fallback driver set for offline installs.

## Issue: Storage controller switch gotchas (boot loops, 0x7B)

**Symptom**

- After switching AHCI → virtio-blk, Windows:
  - reboots repeatedly, or
  - BSODs with `0x0000007B INACCESSIBLE_BOOT_DEVICE`

**Why it happens**

Windows is booting from a disk controller whose driver is not installed or not configured as a boot-start driver. This is the most common failure mode when switching storage controllers.

**Fix**

1. Use the **Safe rollback path** (back to AHCI).
2. From the working AHCI boot:
   - Re-run `setup.cmd` as Administrator.
   - Reboot once (still on AHCI) to let Windows finish driver staging.
3. Switch to virtio-blk again.

**Tip: change one device class at a time**

Do storage first, then network, then GPU. If you change storage + GPU simultaneously and the guest can’t boot or can’t display, recovery becomes much harder.

### Advanced: confirm boot-critical virtio-blk pre-seeding

If you can boot on AHCI but consistently get `0x7B` on virtio-blk, check that Guest Tools actually completed the boot-critical storage setup:

1. Review `C:\AeroGuestTools\install.log` and look for a section like “Preparing boot-critical virtio-blk storage plumbing…”.
2. Confirm the virtio-blk storage driver service is configured as boot-start:
   - Registry:
     - `HKLM\SYSTEM\CurrentControlSet\Services\<storage-service>`
   - Expected values:
     - `Start = 0` (BOOT_START)
     - `ImagePath = system32\drivers\<driver>.sys`
   - The exact service name and expected PCI IDs are defined by the Guest Tools media in `config\devices.cmd`.
3. Confirm CriticalDeviceDatabase entries exist for the expected virtio-blk PCI IDs:
   - `HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase\PCI#VEN_....`
   - These keys map the PCI ID to the storage service so Windows can load the driver early enough to mount the boot volume.

If these entries are missing, re-run `setup.cmd` as Administrator and reboot once on AHCI before switching back to virtio-blk.

## Issue: No bootable device or BOOTMGR is missing after switching storage

**Symptom**

- The VM firmware shows an error like:
  - `No bootable device`, or
  - `BOOTMGR is missing`

**Common causes**

- The disk image is not attached after the hardware profile change.
- Boot order changed and the VM is trying to boot from the wrong device (for example, CD/DVD with no media).
- The disk/controller change did not actually map the existing system disk to the new controller.

**Fix**

1. Power off the VM.
2. Verify the system disk image is still attached.
3. Verify the boot order still prefers the disk.
4. If you can’t quickly resolve it, switch back to the known-good **AHCI** storage controller and boot Windows, then retry the switch to virtio-blk.

## Issue: Virtio device not found or Unknown device after switching

**Symptom**

- After switching to virtio-net or virtio-blk, Windows shows:
  - “Unknown device” in Device Manager, or
  - no working network adapter, or
  - the disk/controller isn’t using the expected driver

**Fix**

1. Confirm the VM is actually configured to use the virtio device (on the host side).
2. In Windows:
   - Open Device Manager → Action → Scan for hardware changes.
3. If the device still shows as unknown:
   - Right-click → Update Driver Software…
   - Browse to the Guest Tools driver folder and ensure you’re selecting the correct architecture.
4. If installation is blocked by signatures, resolve Code 52 first.

## Issue: Lost keyboard/mouse after switching to virtio-input

**Symptom**

- After switching input devices to **virtio-input**, Windows boots but you have no working keyboard/mouse.

**Fix**

1. Power off the VM.
2. Switch input back to **PS/2**.
3. Boot Windows.
4. Re-run `setup.cmd` as Administrator (so the virtio-input driver package is staged).
5. If Device Manager shows signing or driver errors for the input device, resolve them first (Code 52 / Code 28 / Code 10), then switch back to virtio-input.

## Issue: Device Manager Code 28 (drivers not installed)

**Symptom**

- Device Manager shows:
  - `The drivers for this device are not installed. (Code 28)`

**Fix**

1. Run `setup.cmd` as Administrator again (it should stage the missing drivers).
2. Or install the driver manually:
   - Right-click the device → **Update Driver Software…**
   - **Browse my computer for driver software**
   - Point it at your Guest Tools driver folder.

## Issue: Device Manager Code 10 (device cannot start)

**Symptom**

- Device Manager shows:
  - `This device cannot start. (Code 10)`

**Common causes**

- Wrong driver (x86 vs x64, or the wrong device class).
- Driver is present but blocked by signing/trust (sometimes appears as Code 10 or Code 52 depending on the device).
- Incomplete/mismatched driver package (mixed versions).

**Fix**

1. Check signature/trust first (Code 52 section, KB3033929, correct clock).
2. Re-run `setup.cmd` as Administrator.
3. If you recently changed multiple VM devices, roll back and switch one device class at a time to isolate the failure.

## Issue: Windows Setup can't see a virtio-blk disk (slipstream installs)

This only applies if you are attempting to install Windows directly onto **virtio-blk** during Windows Setup.

**Symptom**

- Windows Setup shows “Where do you want to install Windows?” but no disks are listed.

**Cause**

- The virtio-blk storage driver is not available in the Windows Setup environment (`boot.wim`).

**Fix**

- Either:
  - Install Windows using baseline **AHCI** first (recommended), then switch to virtio-blk after running Guest Tools, **or**
  - Attach a driver media disk and use **Load Driver** during Windows Setup:
    - Drivers ISO: browse `drivers\...\x86\` or `drivers\...\x64\` as appropriate
    - FAT driver disk (`*-fat.vhd`): browse `x86\` or `x64\` (see [`docs/16-driver-install-media.md`](./16-driver-install-media.md))
    - Then select the storage driver `.inf` and continue installation, **or**
  - Slipstream the virtio-blk driver into `sources\\boot.wim` (indexes 1 and 2) and rebuild the ISO.

## Issue: `setup.cmd` fails (won't run)

**Common symptoms**

- Double-clicking does nothing.
- You see “Access is denied” / “The requested operation requires elevation”.
- You see a console window that closes immediately.

**Fix**

1. Run Guest Tools from:
   - the mounted CD/DVD (for example `X:\setup.cmd`), **or**
   - a local copy (recommended: `C:\AeroGuestTools\media\setup.cmd`)
2. Right-click `setup.cmd` → **Run as administrator**.
3. If it still fails, run it from an elevated Command Prompt so you can read the output:
   - Start menu → type `cmd` → right-click **cmd.exe** → Run as administrator
   - `cd /d X:\` (or `cd /d C:\AeroGuestTools\media`)
   - `setup.cmd`
   - Review `C:\AeroGuestTools\install.log` afterwards.
4. If the script is incompatible with your build or you need a fallback, use the manual install steps in the Guest Tools guide:
   - [`docs/windows7-guest-tools.md`](./windows7-guest-tools.md#if-setupcmd-fails-manual-install-advanced)

## Issue: Black screen after switching to the Aero GPU

**Symptom**

- After switching **VGA → Aero GPU**, Windows appears to boot but the display is blank/black, or you cannot reach a usable desktop.

**Fix / recovery**

1. Power off the VM.
2. Switch graphics back to **VGA** in the VM settings.
3. Boot Windows.
4. Check Device Manager for the Aero GPU device status:
   - If you see Code 52, fix signing/trust first.
   - If you see an unknown device, reinstall drivers (run `setup.cmd` as Administrator).
5. Try switching to Aero GPU again.

If you must keep the Aero GPU selected while recovering, use Safe Mode (below) since it typically avoids loading third-party display drivers.

### Alternative recovery options (if the OS boots but the screen is unusable)

- Try the boot menu option:
  - **F8** → **Enable low-resolution video (640x480)**
- Or force VGA/base video mode via BCD (from a working boot, typically while still on VGA):
  - Enable:
    - `bcdedit /set {current} basevideo yes`
    - Reboot and retry with the Aero GPU selected
  - Disable (after recovery):
    - `bcdedit /deletevalue {current} basevideo`

## Issue: Aero theme not available (stuck in basic graphics mode)

**Symptoms**

- Only “Windows 7 Basic” / classic themes are available.
- Resolution options are limited (often 800×600) or color depth is wrong.

**Fix**

1. Confirm the Aero GPU driver is actually loaded:
   - Device Manager → Display adapters should show the Aero GPU device without warnings.
2. Run the Windows Experience Index assessment (often enables Aero):
   - Open an elevated Command Prompt and run: `winsat formal`
   - Reboot.
3. Then select an Aero theme:
   - Desktop right-click → Personalize → pick a theme under **Aero Themes**.

## Safe Mode recovery tips

Safe Mode is useful if the system boots but a driver (commonly display) causes instability.

### Option A: Use F8 at boot (legacy boot menu)

If your VM can send F8 early enough during boot:

1. Reboot.
2. Press **F8** repeatedly before the Windows logo appears.
3. Choose **Safe Mode**.

### Option B: Force Safe Mode via `bcdedit` (more reliable)

From a working boot (typically while still on AHCI):

1. Open an elevated Command Prompt.
2. Enable Safe Mode:
   - `bcdedit /set {current} safeboot minimal`
3. Shut down, apply the hardware change (virtio/GPU), and boot.

After you recover, disable Safe Mode:

- `bcdedit /deletevalue {current} safeboot`

### If you got “stuck in Safe Mode”

If you set `safeboot minimal` and forget to remove it, Windows will continue to boot into Safe Mode every time.

Fix (from an elevated Command Prompt):

- `bcdedit /deletevalue {current} safeboot`
- Reboot

### Other useful F8 boot options (Windows 7)

- **Last Known Good Configuration (advanced)**: rolls back to the last driver/service configuration that successfully reached the logon desktop.
- **Enable Boot Logging**: writes `C:\Windows\ntbtlog.txt`, which can help identify which driver loads last before a hang/boot failure.

## Issue: Test Mode watermark on the desktop (x64)

If test signing is enabled, Windows 7 x64 shows a “Test Mode” watermark. This is expected if you are using test-signed drivers.

Only disable test signing if you are sure you have production-signed drivers installed and loading:

- Disable:
  - `bcdedit /set {current} testsigning off`
  - Reboot

If you disable it too early, the drivers may stop loading and devices may fall back to “unknown” or Code 52.
