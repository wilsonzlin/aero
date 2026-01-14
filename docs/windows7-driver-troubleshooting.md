# Windows 7 Driver Troubleshooting (Aero Guest Tools)

This document covers common Windows 7 issues after installing **Aero Guest Tools** and switching the VM from baseline emulated devices (**AHCI/IDE/e1000/VGA**) to paravirtual devices (**virtio + Aero GPU**).

If you have not installed Guest Tools yet, start here:

- [`docs/windows7-guest-tools.md`](./windows7-guest-tools.md)
- For the canonical Windows 7 boot/install storage topology (AHCI HDD + IDE/ATAPI CD-ROM), see
  [`docs/05-storage-topology-win7.md`](./05-storage-topology-win7.md).

## Before you start: quick triage checklist

1. **Don’t keep rebooting** if you hit a boot loop or `0x7B` BSOD after switching storage. Power off and use the rollback path.
2. Collect `report.txt` by running `verify.cmd` as Administrator and opening `C:\AeroGuestTools\report.txt`. Pay special attention to any `Code 52` (signing/trust) or `Code 28` (driver not installed) device errors.
   - Also note the Guest Tools `signing_policy` (from `manifest.json`) and the **Signature Mode (BCDEdit)** check; together they tell you whether Test Signing is expected.
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
  - [Allocation failures (E_OUTOFMEMORY)](#issue-allocation-failures-e_outofmemory)
  - [32-bit D3D9 apps fail on Windows 7 x64 (missing WOW64 UMD)](#issue-32-bit-d3d9-apps-fail-on-windows-7-x64-missing-wow64-umd)
  - [32-bit D3D11 apps fail on Windows 7 x64 (missing WOW64 D3D10/11 UMD)](#issue-32-bit-d3d11-apps-fail-on-windows-7-x64-missing-wow64-d3d1011-umd)
- Guest Tools installation problems:
  - [`setup.cmd` fails (won't run)](#issue-setupcmd-fails-wont-run)
  - [Safe Mode recovery tips](#safe-mode-recovery-tips)
- Expected behavior:
  - [Test Mode watermark on the desktop (x64)](#issue-test-mode-watermark-on-the-desktop-x64)
- Diagnostics:
  - [Collecting useful logs](#collecting-useful-logs)
  - [Dumping the last AeroGPU submission (cmd stream and alloc table)](#dumping-the-last-aerogpu-submission-cmd-stream-and-alloc-table)
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

## Dumping the last AeroGPU submission (cmd stream and alloc table)

If you hit a **GPU hang**, **TDR**, or **incorrect rendering** and need to debug what command stream the guest last submitted (without attaching WinDbg), capture the last submission’s binary blobs and decode them on the host.

This produces one or more small, shareable artifacts:

- `cmd.bin`: the raw AeroGPU cmd stream for the submission (`cmd_gpa` region).
- `alloc.bin` (or `cmd.bin.alloc_table.bin`): the raw alloc table for the submission (`alloc_table_gpa` region, when present; AGPU only).
- `cmd.bin.txt`: a small text summary (ring index, fence, GPAs/sizes).

Also capture a one-shot dbgctl snapshot (recommended):

- `aerogpu_dbgctl.exe --status` (or `--status --json=C:\status.json`)
  - Includes fences/ring state and the most recent **latched device error** (`Last error:`) when supported (ABI 1.3+ / `AEROGPU_IRQ_ERROR`).

### 1) Guest (Windows 7): dump the last submission
  
Run this inside the guest as soon as possible after reproducing:

From a default Aero Guest Tools ISO/zip mount (often `X:`), run the command that matches your guest OS:

- Win7 x64:

```bat
X:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --dump-last-cmd --out C:\cmd.bin
```

- Win7 x86:

```bat
X:\drivers\x86\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --dump-last-cmd --out C:\cmd.bin
```

Equivalent newer spelling (same behavior; accepted by newer dbgctl builds; assumes you're running from the dbgctl directory or have it on `PATH`):

```bat
aerogpu_dbgctl.exe --dump-last-submit --cmd-out C:\cmd.bin
```

Notes on legacy spellings (kept for compatibility with older dbgctl builds):

- For `aerogpu_dbgctl.exe`, `--dump-last-cmd` is an alias for `--dump-last-submit`.
- For `aerogpu_dbgctl.exe --dump-last-submit`, `--out` is an alias for `--cmd-out`.

For `aerogpu_dbgctl.exe --dump-last-submit`, if you want a stable alloc-table output filename (`C:\alloc.bin`) instead of the default `C:\cmd.bin.alloc_table.bin` (AGPU only), set `--alloc-out` (only supported when dumping a single submission, i.e. `--count 1`):

```bat
:: Replace <GuestToolsDrive> with the drive letter of the mounted Guest Tools ISO/zip (e.g. D).
:: Win7 x64:
cd /d <GuestToolsDrive>:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin
:: Win7 x86:
:: cd /d <GuestToolsDrive>:\drivers\x86\aerogpu\tools\win7_dbgctl\bin

aerogpu_dbgctl.exe --dump-last-submit --cmd-out C:\cmd.bin --alloc-out C:\alloc.bin
```

Then copy `C:\cmd.bin`, `C:\cmd.bin.txt`, and any alloc-table dump that `aerogpu_dbgctl` produced (`C:\alloc.bin` if you passed `--alloc-out`, or `C:\cmd.bin.alloc_table.bin` when present) to the host machine (shared folder, ISO, whatever is convenient).

Notes:

- On Win7 x64, `aerogpu_dbgctl.exe` is intentionally an **x86 (32-bit)** binary and runs under **WOW64**
  (even when invoked from `X:\drivers\amd64\...`), so you can run it directly from the mounted media without editing `PATH`.
- This requires the installed KMD to allow the debug-only `AEROGPU_ESCAPE_OP_READ_GPA` escape.
  - If `READ_GPA` is not enabled/authorized, dbgctl will fail with `STATUS_NOT_SUPPORTED` (`0xC00000BB`).
  - To enable it, set (and reboot/restart the driver):  
    `HKLM\SYSTEM\CurrentControlSet\Services\aerogpu\Parameters\EnableReadGpaEscape = 1` (REG_DWORD)  
    and run dbgctl as a privileged user (Administrator and/or `SeDebugPrivilege`).
- If `aerogpu_dbgctl` refuses to dump due to the default size cap (1 MiB), re-run with `--force`:
  - `aerogpu_dbgctl.exe --dump-last-submit --cmd-out C:\cmd.bin --alloc-out C:\alloc.bin --force`
- For `aerogpu_dbgctl.exe --dump-last-submit`, to capture an older submission (for example if the newest submit is a tiny no-op), use `--index-from-tail`:
  - `aerogpu_dbgctl.exe --dump-last-submit --index-from-tail 1 --cmd-out C:\prev_cmd.bin --alloc-out C:\prev_alloc.bin`
- For `aerogpu_dbgctl.exe --dump-last-submit`, to dump multiple recent submissions in one run, use `--count N` (writes one output per submission, like `cmd_0.bin`, `cmd_1.bin`, ...).
  - Note: for `aerogpu_dbgctl --dump-last-submit`, `--alloc-out` is only supported when dumping a single submission (`--count 1`). When dumping multiple submissions, `aerogpu_dbgctl` writes alloc tables (when present) to `<cmd_path>.alloc_table.bin` next to each dumped cmd stream.
  - `aerogpu_dbgctl.exe --dump-last-submit --count 4 --cmd-out C:\cmd.bin`
- For `aerogpu_dbgctl.exe`, if your build uses multiple rings, select the ring with `--ring-id N` (default is 0).

### 2) Host: decode the submission

From the repo root on the host:

If `alloc.bin` exists (or you have `cmd.bin.alloc_table.bin`), decode the full submission (replace the `--alloc` path as needed):

```bash
cargo run -p aero-gpu-trace-replay -- decode-submit --cmd cmd.bin --alloc alloc.bin
```

To inspect the alloc table itself (alloc_id → gpa/size/flags), run (replace the `alloc.bin` path as needed):

```bash
cargo run -p aero-gpu-trace-replay -- decode-alloc-table alloc.bin
```

If there is no alloc table dump (common on legacy ring formats), skip `decode-submit` and use `decode-cmd-stream` below.

### 3) Optional: list opcodes directly (`decode-cmd-stream`)

You can also decode just the cmd stream to get a stable per-packet opcode listing:

```bash
cargo run -p aero-gpu-trace-replay -- decode-cmd-stream cmd.bin
```

Tip: pass `--strict` to fail on unknown opcodes instead of printing them as `Unknown`:

```bash
cargo run -p aero-gpu-trace-replay -- decode-cmd-stream --strict cmd.bin
```

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

If you ran `setup.cmd /skipstorage` (check for `C:\AeroGuestTools\storage-preseed.skipped.txt`), storage pre-seeding was intentionally skipped. In that case, do **not** switch the boot disk to virtio-blk until you re-run `setup.cmd` **without** `/skipstorage` using Guest Tools media that includes the virtio-blk storage driver.

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

1. **Confirm signature mode (x64):**
   - Run `verify.cmd` and check:
     - `signing_policy` (from the Guest Tools `manifest.json`)
     - **Signature Mode (BCDEdit)** (`testsigning` / `nointegritychecks`)
   - General guidance:
     - If `signing_policy=test`: ensure `testsigning` is **on**.
     - If `signing_policy=production` (WHQL/prod-signed drivers): ensure `testsigning` is **off**.
       - `verify.cmd` will warn if production builds are running in Test Mode.
   - Open an elevated Command Prompt and run:
      - `bcdedit /enum {current}`
   - Look for `testsigning Yes`.
   - If needed, enable or disable it:
      - `bcdedit /set {current} testsigning on`
      - or: `bcdedit /set {current} testsigning off`
      - Reboot.

2. **Confirm the driver signing certificate is installed (recommended for test-signed/custom-signed drivers):**
    - Run `certlm.msc` (Local Computer certificate manager).
    - Check:
      - **Trusted Root Certification Authorities → Certificates**
      - **Trusted Publishers → Certificates**
    - If the certificate is missing, re-run `setup.cmd` as Administrator.
    - Note: If you are using WHQL/production-signed drivers and your Guest Tools media has `signing_policy=production` (or `none`), the media may not ship any `certs\*.cer/*.crt/*.p7b`, and installing a custom certificate is typically unnecessary.

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
4. (Optional) Validate the new media before installing:
   - `setup.cmd /check /verify-media`
5. Re-run `setup.cmd` as Administrator (or use the manual install fallback).

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
2. Ensure you copy/extract **the entire media root** (including `drivers\`, `config\`, and `certs\` when present/required by `manifest.json` `signing_policy`).
3. (Optional) Validate the new media before installing:
   - `setup.cmd /check /verify-media`
4. Re-run `setup.cmd` as Administrator after replacing the media.

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

If you installed Guest Tools with `setup.cmd /skipstorage`, storage pre-seeding was skipped by design. Re-run `setup.cmd` without `/skipstorage` using media that includes the virtio-blk driver before attempting to boot from virtio-blk.

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
- The VM is trying to boot from the wrong device (for example, CD/DVD with no media).
- The disk/controller change did not actually map the existing system disk to the new controller.

**Fix**

1. Power off the VM.
2. Verify the system disk image is still attached.
3. Verify the VM is configured to boot from the disk.
   - Aero note: in Aero’s BIOS, this corresponds to booting from the first HDD (`DL=0x80`). Ensure
     the host/runtime sets the boot drive accordingly (e.g. `Machine::set_boot_drive(0x80)` then
     `reset()`).
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
5. Verify the virtio-input PCI device matches the Aero Win7 virtio contract v1 identity:
    - In Device Manager → the virtio-input PCI device → Properties → Details → **Hardware Ids**
    - The list should include a contract v1 **revision-gated** ID (`REV_01`), such as:
      - `PCI\VEN_1AF4&DEV_1052&REV_01`
      - (Windows will also list less-specific variants such as `PCI\VEN_1AF4&DEV_1052`; this is normal.)
    - If the device exposes Aero subsystem IDs, the list will also include more specific variants, for example:
      - `PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01` *(keyboard)*, or
      - `PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01` *(mouse)*
      - When those subsystem IDs are present, Windows will prefer the more specific match so the devices show up as
        **Aero VirtIO Keyboard** / **Aero VirtIO Mouse**.
    - Note: the canonical in-tree virtio-input INF (`aero_virtio_input.inf`) includes:
      - subsystem-qualified keyboard/mouse model lines (`SUBSYS_0010` / `SUBSYS_0011`) for distinct Device Manager names, and
      - the strict revision-gated generic fallback model line (no `SUBSYS`): `PCI\VEN_1AF4&DEV_1052&REV_01`.
      If your virtio-input PCI device does **not** expose Aero subsystem IDs, Windows can still bind via the fallback entry
      (Device Manager name: **Aero VirtIO Input Device**).
    - If Windows still does not bind the driver, check:
      - the device reports `REV_01` (not `REV_00`), and
      - the driver package is staged/installed (re-run `setup.cmd`), and
      - signing/trust issues (Code 52 / KB3033929 / correct clock), and
      - you are installing the correct architecture (x86 vs x64).
    - Tablet devices bind via the separate tablet INF (`aero_virtio_tablet.inf`,
      `PCI\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01`). That match is more specific than the generic fallback, so it wins when
      both driver packages are installed and it matches. If the tablet INF is not installed, the generic fallback entry can also
      bind to tablet devices (but will use the generic device name).
    - Legacy INF basename note: `virtio-input.inf.disabled` is a disabled-by-default **filename-only alias** for
      `aero_virtio_input.inf`. You may locally rename it to `virtio-input.inf` if a workflow/tool expects that basename.
      - Sync policy: from the first section header (`[Version]`) onward it must be strictly byte-for-byte identical to
        `aero_virtio_input.inf` (banner/comments may differ). See `drivers/windows7/virtio-input/scripts/check-inf-alias.py`.
      - Because it is identical, enabling the alias does **not** change HWID matching behavior (and is not required for
        fallback binding).
      - Avoid installing both basenames at the same time (duplicate packages can cause confusing driver store state/selection).
    - If the device reports `REV_00`, the in-tree Aero virtio-input INFs will not bind; ensure your emulator/QEMU config sets
      `x-pci-revision=0x01` (and preferably `disable-legacy=on`).
6. If Device Manager shows signing or driver errors for the input device, resolve them first (Code 52 / Code 28 / Code 10), then switch back to virtio-input.

For the consolidated end-to-end virtio-input validation plan (Rust device model + Win7 driver + web runtime routing), see:

- [`docs/virtio-input-test-plan.md`](./virtio-input-test-plan.md)

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
- Driver bound but refused to start due to a contract/runtime mismatch (for example a virtio device reporting the wrong `REV_..`).

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
  - Slipstream the virtio-blk driver into `sources\boot.wim` (indexes 1 and 2) and rebuild the ISO.

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

### Advanced diagnostics (scanout / cursor state)

If the OS boots far enough that you can run tools (local console preferred; RDP may change the active display path), dump the scanout state:

`aerogpu_dbgctl.exe` is shipped under the AeroGPU driver directory in packaged outputs:

- Guest Tools ISO/zip (often mounted as `X:`):
 - x64: `X:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`
 - x86: `X:\drivers\x86\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`
 - Optional top-level tools payload (when present): `X:\tools\aerogpu_dbgctl.exe` (or under `X:\tools\<arch>\aerogpu_dbgctl.exe`)
- CI-staged packages (host-side): `out\packages\aerogpu\x64\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe` (and `...\x86\...`)

Example (Guest Tools ISO/zip often mounted as `X:`; replace `X:` with your actual drive letter):

```bat
cd /d X:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin
aerogpu_dbgctl.exe --status
```

In the commands below, `aerogpu_dbgctl.exe` assumes you are running from the directory containing dbgctl (otherwise replace it with a full path).

- `aerogpu_dbgctl.exe --status`
  - Captures a combined snapshot (device/ABI + fences + ring0 + scanout0 + cursor + vblank + CreateAllocation trace summary).

- `aerogpu_dbgctl.exe --query-scanout`
  - Confirms whether scanout is enabled, the current mode (`width/height/pitch`), and whether a framebuffer GPA is programmed.
  - Useful for diagnosing blank output caused by mode/pitch mismatches or a missing scanout surface address.

- `aerogpu_dbgctl.exe --dump-scanout-bmp C:\\scanout.bmp`
  - Dumps the scanout framebuffer to an uncompressed 32bpp BMP (requires the installed KMD to allow the debug-only `AEROGPU_ESCAPE_OP_READ_GPA` escape; see `drivers/aerogpu/tools/win7_dbgctl/README.md`).
  - Useful when the guest “seems alive” but the screen is blank/corrupted and you need a pixel artifact without relying on host-side capture.

- `aerogpu_dbgctl.exe --dump-scanout-png C:\\scanout.png`
  - Same as `--dump-scanout-bmp`, but writes a PNG (RGBA8).
  - Note: dbgctl’s built-in PNG encoder uses stored (uncompressed) deflate blocks for simplicity, so the PNG may be slightly **larger** than the BMP.

- `aerogpu_dbgctl.exe --query-cursor`
  - Dumps the hardware cursor MMIO state (`CURSOR_*` registers): enable, position/hotspot, size/format/pitch, and the cursor framebuffer GPA.
  - Useful when the desktop is running but the cursor is missing/stuck/off-screen.

- `aerogpu_dbgctl.exe --dump-cursor-bmp C:\\cursor.bmp`
  - Dumps the current cursor image to an uncompressed 32bpp BMP (requires the installed KMD to allow the debug-only `AEROGPU_ESCAPE_OP_READ_GPA` escape; see `drivers/aerogpu/tools/win7_dbgctl/README.md`).
  - Useful for debugging cursor image/pitch/fb_gpa issues without relying on host-side capture.

- `aerogpu_dbgctl.exe --dump-cursor-png C:\\cursor.png`
  - Same as `--dump-cursor-bmp`, but writes a PNG (RGBA8; preserves alpha).

If you have the Win7 guest-side validation suite available, you can also run:

- `drivers\\aerogpu\\tests\\win7\\bin\\scanout_state_sanity.exe`
  - Validates that the KMD cached mode matches the MMIO scanout registers and the desktop resolution (helps catch broken `DxgkDdiCommitVidPn` mode caching).

- `drivers\\aerogpu\\tests\\win7\\bin\\cursor_state_sanity.exe`
  - Moves the cursor, sets a custom cursor shape, and validates cursor MMIO state via `AEROGPU_ESCAPE_OP_QUERY_CURSOR`.
  - Note: this test is only meaningful on a local console session; it will skip under RDP unless `--allow-remote` is passed (in which case it still skips).

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

## Issue: 32-bit D3D9 apps fail on Windows 7 x64 (missing WOW64 UMD)

**Symptoms**

- 64-bit D3D apps work (or the desktop is usable), but **32-bit** D3D9 apps fail to start or fail to create a device.
- Common errors include failures from `Direct3DCreate9` / `CreateDevice` in 32-bit apps.

**Why it happens**

On Windows 7 x64, the display driver package must install **both**:

- a 64-bit D3D9 UMD to `C:\Windows\System32\` (despite the name, `System32` is the **64-bit** system directory on x64), and
- a 32-bit (WOW64) D3D9 UMD to `C:\Windows\SysWOW64\` (`SysWOW64` holds the **32-bit** system DLLs on x64).

If the `SysWOW64` UMD is missing, **32-bit apps will not be able to use D3D9** even though 64-bit apps may work.

**Fix**

1. Confirm the expected UMD files exist on the guest:
    - `C:\Windows\System32\aerogpu_d3d9_x64.dll`
    - `C:\Windows\SysWOW64\aerogpu_d3d9.dll`
    - Tip: `verify.cmd` reports this under **AeroGPU D3D9 UMD DLL placement**.
2. Run the guest-side D3D validation suite (recommended) to confirm the *runtime* actually loads the correct UMD DLL:
    - `drivers\aerogpu\tests\win7\run_all.cmd --require-umd`
    - Or just the D3D9 test:
      - `drivers\aerogpu\tests\win7\bin\d3d9ex_triangle.exe --require-umd`
    - The test output should include the resolved UMD path. For a 32-bit test binary on a Win7 x64 guest it should be tagged as `(WOW64)` and typically resolve to `C:\Windows\SysWOW64\aerogpu_d3d9.dll`.
3. If the `SysWOW64` DLL is missing, reinstall using the supported AeroGPU Win7 package:
    - `drivers/aerogpu/packaging/win7/README.md`
    - Ensure your build/staging workflow includes the WOW64 UMD in the **x64** package:
      - If you are using CI-produced packages, `out/packages/aerogpu/x64/` should contain both `aerogpu_d3d9_x64.dll` and `aerogpu_d3d9.dll`.
      - If you are staging from a repo-local build, use `drivers\aerogpu\build\stage_packaging_win7.cmd fre x64`.
4. Reboot the guest after reinstalling the display driver.

## Issue: 32-bit D3D11 apps fail on Windows 7 x64 (missing WOW64 D3D10/11 UMD)

**Symptoms**

- 64-bit D3D10/D3D11 apps work, but **32-bit** D3D10/D3D11 apps fail to start or fail to create a device.
- Common failures show up in 32-bit apps calling `D3D10CreateDevice*` / `D3D11CreateDevice*` (often `E_FAIL` / `DXGI_ERROR_UNSUPPORTED`), or the app may crash during device creation if the runtime can’t load the expected UMD.

**Why it happens**

If you install the DX11-capable AeroGPU driver package (`aerogpu_dx11.inf`) on Windows 7 x64, the package must install **both**:

- a 64-bit D3D10/11 UMD to `C:\Windows\System32\`:
  - `C:\Windows\System32\aerogpu_d3d10_x64.dll`
- a 32-bit (WOW64) D3D10/11 UMD to `C:\Windows\SysWOW64\`:
  - `C:\Windows\SysWOW64\aerogpu_d3d10.dll`

The UMD filenames are also registered in the adapter’s registry key:

- `UserModeDriverName = "aerogpu_d3d10_x64.dll"` (native x64)
- `UserModeDriverNameWow = "aerogpu_d3d10.dll"` (WOW64 x86)

If the WOW64 UMD is missing or not registered, **32-bit D3D10/D3D11 apps will not be able to use AeroGPU** even though 64-bit apps may work.

**Fix**

1. Confirm the expected UMD files exist on the guest:
   - `C:\Windows\System32\aerogpu_d3d10_x64.dll`
   - `C:\Windows\SysWOW64\aerogpu_d3d10.dll`
   - Tip: `verify.cmd` reports this under **AeroGPU D3D10/11 UMD DLL placement** (if any D3D10/11 UMD DLLs are detected).
2. Confirm the UMD registry values:
   - From a DX11-capable driver package, run:
     - `drivers\aerogpu\packaging\win7\verify_umd_registration.cmd dx11`
   - This prints and validates `UserModeDriverName` / `UserModeDriverNameWow`.
3. Run the guest-side D3D validation suite (recommended) to confirm the runtime loads the correct UMD DLL:
   - `drivers\aerogpu\tests\win7\run_all.cmd --require-umd`
   - Or just the D3D11 test:
     - `drivers\aerogpu\tests\win7\bin\d3d11_triangle.exe --require-umd`
4. Reboot the guest after reinstalling the display driver.

## Issue: Allocation failures (E_OUTOFMEMORY)

**Symptoms**

- D3D9/D3D10/D3D11 apps fail to create resources (textures, buffers, swapchain backbuffers), often returning:
  - `E_OUTOFMEMORY`
  - `D3DERR_OUTOFVIDEOMEMORY`
- The failures may occur “too early” (for example, after allocating only a few large textures) even though the guest still has free RAM.

**Why it happens**

AeroGPU is a **system-memory-backed** WDDM adapter (no dedicated VRAM). (The device may still expose
BAR1 as a legacy VGA/VBE compatibility aperture.) Even so, the Windows 7 graphics kernel (`dxgkrnl`) enforces
a per-adapter **segment budget** based on what the KMD reports as “non-local” memory.

The AeroGPU Win7 KMD defaults this budget to **512 MB** for bring-up. Some workloads legitimately need a larger budget, otherwise
allocations can fail due to the budget limit rather than actual guest memory exhaustion.

**Fix: increase the segment budget hint (`NonLocalMemorySizeMB`)**

Set the AeroGPU device registry parameter:

- **Key:** `HKR\Parameters\NonLocalMemorySizeMB`
- **Type:** `REG_DWORD`
- **Unit:** MB
- **Default:** 512
- **Clamped:** min 128; max 2048 on x64; max 1024 on x86

Recommended starting points:

- **Win7 x64:** 1024–2048 (depending on guest RAM and workload)
- **Win7 x86:** 256–1024 (larger values are clamped to 1024)

Important: this is a **budget hint** (system-RAM-backed), not dedicated VRAM. It does not “create VRAM”; it only changes what the
driver reports to dxgkrnl. Setting it too high can increase guest RAM consumption and paging pressure under heavy workloads.

### How to set it (Win7)

1. Find the AeroGPU adapter driver key:
   - Device Manager → Display adapters → AeroGPU → Properties → Details → select **Driver key**.
   - It typically looks like:
     - `HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\000X\`
2. Create/open a `Parameters` subkey and set `NonLocalMemorySizeMB`. Example (replace `000X`):

```bat
reg add "HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\000X\Parameters" ^
  /v NonLocalMemorySizeMB /t REG_DWORD /d 2048 /f
```

3. Reboot the guest (or disable/enable the AeroGPU device) for the new budget to take effect.

Quick validation:

- If you have the guest-side AeroGPU validation suite available, run `drivers\\aerogpu\\tests\\win7\\bin\\segment_budget_sanity.exe`
  to confirm the updated `NonLocalMemorySize` is visible from user mode (it queries `D3DKMTQueryAdapterInfo(GETSEGMENTGROUPSIZE)`
  and prints the segment budget in MiB).
- Re-run `verify.cmd` and check `C:\AeroGuestTools\report.txt` / `report.json` for the AeroGPU `NonLocalMemorySizeMB` value
  (to confirm the override is present and what value is configured).

If you need to revert, delete the value:

```bat
reg delete "HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\000X\Parameters" ^
  /v NonLocalMemorySizeMB /f
```

For the canonical KMD-side behavior and rationale, see: `drivers/aerogpu/kmd/README.md`.

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

Only disable test signing if you are sure you have production-signed drivers installed and loading (for example, `verify.cmd` reports `signing_policy=production`):

- Disable:
  - `bcdedit /set {current} testsigning off`
  - Reboot

If you disable it too early, the drivers may stop loading and devices may fall back to “unknown” or Code 52.
