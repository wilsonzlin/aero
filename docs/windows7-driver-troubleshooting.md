# Windows 7 Driver Troubleshooting (Aero Guest Tools)

This document covers common Windows 7 issues after installing **Aero Guest Tools** and switching the VM from baseline emulated devices (**AHCI/e1000/VGA**) to paravirtual devices (**virtio + Aero GPU**).

If you have not installed Guest Tools yet, start here:

- [`docs/windows7-guest-tools.md`](./windows7-guest-tools.md)

## Before you start: quick triage checklist

1. **Don’t keep rebooting** if you hit a boot loop or `0x7B` BSOD after switching storage. Power off and use the rollback path.
2. Collect `report.txt` by running `verify.cmd` as Administrator (from your `C:\AeroGuestTools\` copy).
3. Confirm you’re using drivers that match your OS:
   - Windows 7 **x86** requires x86 drivers.
   - Windows 7 **x64** requires x64 drivers. (32-bit drivers cannot load.)
4. If you changed multiple VM devices at once (storage + GPU + network), consider rolling back and switching **one class at a time** so failures are easier to isolate.
5. Confirm the guest **date/time** is correct. If the clock is far off, Windows may treat certificates as “not yet valid” or “expired” and driver signature validation can fail.

## Safe rollback path (storage boot failure)

If Windows fails to boot after switching the system disk from **AHCI → virtio-blk**:

1. Power off the VM.
2. Switch the disk controller back to **AHCI**.
3. Boot Windows (it should boot again).
4. Re-run `setup.cmd` as Administrator and reboot once on AHCI.
5. Try switching to virtio-blk again.

Why this works: Windows can only boot from a storage controller if its driver is installed and configured as boot-critical. Going back to AHCI restores the known-good boot path so you can fix the driver configuration from inside Windows.

## Issue: Device Manager “Code 52” (signature / trust failures)

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
     - Right-click the device → Update Driver Software
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

If the update fails to install, ensure you are on Windows 7 SP1 and consider installing the latest Windows 7 servicing stack update first.

### Recommended signing algorithm policy (for compatibility)

If you are producing or selecting driver packages for Windows 7:

- **Best out-of-box compatibility:** SHA-1-signed catalogs (works on a fresh SP1 install).
- **SHA-256-only signing:** requires KB3033929 (and users frequently don’t have it offline).
- **Practical approach:** provide a path that works both ways:
  - Ensure Guest Tools clearly tells users when KB3033929 is required, and/or
  - Provide a SHA-1-signed fallback driver set for offline installs.

## Issue: Storage controller switch gotchas (boot loops / 0x7B)

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

## Issue: Virtio device “not found” / Unknown device after switching

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

## Issue: Windows Setup can’t see a virtio-blk disk (slipstream installs)

This only applies if you are attempting to install Windows directly onto **virtio-blk** during Windows Setup.

**Symptom**

- Windows Setup shows “Where do you want to install Windows?” but no disks are listed.

**Cause**

- The virtio-blk storage driver is not available in the Windows Setup environment (`boot.wim`).

**Fix**

- Either:
  - Install Windows using baseline **AHCI** first (recommended), then switch to virtio-blk after running Guest Tools, **or**
  - Slipstream the virtio-blk driver into `sources\\boot.wim` (indexes 1 and 2) and rebuild the ISO.

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

## Issue: Aero theme not available / stuck in basic graphics mode

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

## Issue: “Test Mode” watermark on the desktop (x64)

If test signing is enabled, Windows 7 x64 shows a “Test Mode” watermark. This is expected if you are using test-signed drivers.

Only disable test signing if you are sure you have production-signed drivers installed and loading:

- Disable:
  - `bcdedit /set {current} testsigning off`
  - Reboot

If you disable it too early, the drivers may stop loading and devices may fall back to “unknown” or Code 52.
