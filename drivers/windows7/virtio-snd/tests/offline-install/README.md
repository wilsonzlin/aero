<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Offline / slipstream install: virtio-snd driver into Windows 7 (WIM or offline OS)

This document describes how to **stage** (preinstall) the Aero `virtio-snd` Windows 7 driver so that Plug-and-Play can bind it **automatically on first boot**.

This is useful when you want a Win7 image that comes up with the virtio-snd PCI device already installed (for example: unattended installs, automated test images, or prebuilt VHD/VHDX disks).

The driver package you inject must ultimately be a normal driver folder containing at least:

```text
# Aero contract v1 (modern-only, strict):
aero_virtio_snd.inf
aero_virtio_snd.sys
aero_virtio_snd.cat   (recommended; required for unattended Win7 x64)

# QEMU transitional (optional):
aero-virtio-snd-legacy.inf
virtiosnd_legacy.sys
aero-virtio-snd-legacy.cat   (recommended; required for unattended Win7 x64)
```

In this repo, the packaging/staging directory is:

```text
drivers/windows7/virtio-snd/inf/
```

In packaged driver bundles (ZIP/ISO), the same driver package payload files are located under:

```text
drivers\virtio-snd\x86\   (Windows 7 x86)
drivers\virtio-snd\x64\   (Windows 7 x64)
```

Note: the default CI/Guest Tools bundle includes only the Aero contract v1 package
(`aero_virtio_snd.inf` + `aero_virtio_snd.sys`). The optional transitional/QEMU package
(`aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`) must be built and staged separately.

> `virtio-snd` is **not boot-critical** (it’s a PnP media device, StartType=3). If staging fails or the driver is blocked by signature policy at runtime, Windows should still boot; you’ll just have an unbound device to troubleshoot.

---

## Prerequisites

- A Windows host with `dism.exe` available:
  - Windows 7: built-in DISM
  - Windows 10/11: built-in DISM (usually works fine for servicing Win7 WIMs)
- An **elevated** Command Prompt (`Run as administrator`)
- A writable working directory with enough free space (mounting a WIM expands it)
- Windows 7 install media extracted to a folder (or copied from a USB stick)

Notes:

- If your install media is mounted as a read-only ISO, copy `install.wim` somewhere writable before servicing.
- The main goal is staging into `install.wim` (the installed OS). Injecting into `boot.wim` is optional; see [Optional: inject into `boot.wim` (WinPE/Setup)](#optional-inject-into-bootwim-winpesetup).

---

## Choose the correct driver architecture (x86 vs x64)

You must inject a driver package that matches the target Windows 7 architecture:

- Windows 7 **x86** → use an x86 build of `aero_virtio_snd.sys`
- Windows 7 **x64** → use an amd64/x64 build of `aero_virtio_snd.sys`

`aero_virtio_snd.inf` has `NTx86` and `NTamd64` models, but the binary filename is the same (`aero_virtio_snd.sys`) for both architectures. To avoid mixing them up, it’s easiest to keep separate per-arch folders (example only):

```text
C:\drivers\virtio-snd\x86\   (aero_virtio_snd.inf + aero_virtio_snd.sys (x86) + aero_virtio_snd.cat)
C:\drivers\virtio-snd\amd64\ (aero_virtio_snd.inf + aero_virtio_snd.sys (x64) + aero_virtio_snd.cat)
```

For the transitional/QEMU package, keep separate per-arch folders for `virtiosnd_legacy.sys` as well.

Point DISM at the folder (or the `.inf`) for the specific architecture you are servicing.

---

## Option A: Slipstream into Windows 7 install media (`install.wim`)

### 1) Identify which image index you will install

`install.wim` commonly contains multiple editions. List them:

```bat
set WIM=C:\win7\sources\install.wim
dism /Get-WimInfo /WimFile:%WIM%
```

Pick the `Index` for the edition you will actually install.

### 2) Mount the WIM

```bat
set MOUNT=C:\wim\mount
mkdir %MOUNT%

REM Example mounts index 1; replace with your chosen index.
dism /Mount-Wim /WimFile:%WIM% /Index:1 /MountDir:%MOUNT%
```

### 3) Add (stage) the virtio-snd driver

Set `VIRTIO_SND_INF_DIR` to a folder containing `aero_virtio_snd.inf` + `aero_virtio_snd.sys` for the correct architecture.

Example (repo checkout at `C:\src\aero`):

```bat
set REPO=C:\src\aero

REM Point this at a folder containing the desired INF + SYS for the correct architecture:
REM   - Aero contract v1: aero_virtio_snd.inf + aero_virtio_snd.sys
REM   - QEMU transitional: aero-virtio-snd-legacy.inf + virtiosnd_legacy.sys
set VIRTIO_SND_INF_DIR=%REPO%\drivers\windows7\virtio-snd\inf

dism /Image:%MOUNT% /Add-Driver /Driver:%VIRTIO_SND_INF_DIR% /Recurse
```

Example (bundle extracted to `C:\aero-drivers\`):

```bat
REM Use x64 for Windows 7 x64; use x86 for Windows 7 x86.
set VIRTIO_SND_INF_DIR=C:\aero-drivers\drivers\virtio-snd\x64
dism /Image:%MOUNT% /Add-Driver /Driver:%VIRTIO_SND_INF_DIR% /Recurse
```

If you are building a test-only image and don’t have a trusted signature yet, DISM can be forced to stage the package:

```bat
dism /Image:%MOUNT% /Add-Driver /Driver:%VIRTIO_SND_INF_DIR% /Recurse /ForceUnsigned
```

See [Driver signing / test-signing warnings](#driver-signing--test-signing-warnings) before relying on `/ForceUnsigned` for Windows 7 x64.

### 4) Verify the driver is staged in the offline DriverStore

List staged 3rd-party drivers in the mounted image:

```bat
dism /Image:%MOUNT% /Get-Drivers /Format:Table
```

Locate the `oem#.inf` entry corresponding to `aero_virtio_snd.inf` (or `aero-virtio-snd-legacy.inf`), then inspect it:

```bat
dism /Image:%MOUNT% /Get-DriverInfo /Driver:oem#.inf
```

(Optional) The package should also appear under:

```text
%MOUNT%\Windows\System32\DriverStore\FileRepository\
```

### 5) Commit changes and unmount

```bat
dism /Unmount-Wim /MountDir:%MOUNT% /Commit
```

To discard changes instead:

```bat
dism /Unmount-Wim /MountDir:%MOUNT% /Discard
```

If DISM reports a stale mount from a previous run:

```bat
dism /Cleanup-Wim
```

### 6) Use the updated `install.wim`

- If you serviced `install.wim` in place under your extracted install media folder, you’re done.
- If you serviced a copy, copy it back to `...\sources\install.wim` before creating bootable media.

On first boot of the installed OS, Windows will enumerate the virtio-snd PCI device and should automatically select the best matching driver from the offline DriverStore.

---

## Option B: Inject into an already-installed offline Windows directory (mounted disk)

This is useful if you already have a Windows 7 VM disk image and want the driver present on next boot without reinstalling.

1) Mount the disk so it shows up as a drive letter (example uses DiskPart; Disk Management GUI also works):

```bat
diskpart
DISKPART> select vdisk file="C:\vm\win7.vhd"
DISKPART> attach vdisk
DISKPART> list volume
DISKPART> exit
```

Identify the volume letter that contains `\Windows\` (example uses `W:`).

2) Add the driver to the offline Windows installation:

```bat
set OFFLINE=W:\
set REPO=C:\src\aero
set VIRTIO_SND_INF_DIR=%REPO%\drivers\windows7\virtio-snd\inf

dism /Image:%OFFLINE% /Add-Driver /Driver:%VIRTIO_SND_INF_DIR% /Recurse
```

Example (bundle extracted to `C:\aero-drivers\`):

```bat
set OFFLINE=W:\
REM Use x64 for Windows 7 x64; use x86 for Windows 7 x86.
set VIRTIO_SND_INF_DIR=C:\aero-drivers\drivers\virtio-snd\x64
dism /Image:%OFFLINE% /Add-Driver /Driver:%VIRTIO_SND_INF_DIR% /Recurse
```

3) Verify it’s staged:

```bat
dism /Image:%OFFLINE% /Get-Drivers /Format:Table
```

4) Detach the disk when done:

```bat
diskpart
DISKPART> select vdisk file="C:\vm\win7.vhd"
DISKPART> detach vdisk
DISKPART> exit
```

On the next boot, Plug-and-Play should bind the device to the staged driver.

---

## Verification after first boot (inside Windows 7)

Once the system boots with virtio-snd hardware present:

- **Device Manager** (`devmgmt.msc`)
  - Under **Sound, video and game controllers**, you should see the virtio-snd device (the INF default name is **“Aero VirtIO Sound Device”**).
  - In **Properties → Driver → Driver Details**, you should see `aero_virtio_snd.sys` (contract v1) or `virtiosnd_legacy.sys` (transitional/QEMU).
  - You should also see playback/recording endpoints under **Audio inputs and outputs**.
- **PnP driver store**

  `pnputil -e` lists staged packages:

  ```cmd
  pnputil -e
  ```

  Look for the entry whose “Original name” is `aero_virtio_snd.inf` (the “Published name” will be `oem#.inf`).

- **SetupAPI log**

  Inspect `%WINDIR%\inf\setupapi.dev.log` and search for:

  - `aero_virtio_snd.inf` (or `aero-virtio-snd-legacy.inf`), or
  - the device hardware ID (for virtio-snd contract v1: `PCI\VEN_1AF4&DEV_1059&REV_01`; more-specific `SUBSYS_...` qualifiers may also appear).
    If you see `DEV_1018`, the device is transitional and the Aero contract v1 INF will not bind.

  Note: `aero_virtio_snd.inf` is intentionally strict and matches only `PCI\VEN_1AF4&DEV_1059&REV_01`.
  If the device does not expose that HWID (for example it reports `REV_00` under QEMU defaults), either:
  
  - If the device enumerates as transitional virtio-snd (`PCI\VEN_1AF4&DEV_1018`; common for stock QEMU defaults),
    inject/install the legacy package (`aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`).
  - Otherwise, configure QEMU to expose the contract identity (for example `disable-legacy=on,x-pci-revision=0x01`).

 If the driver package is staged but the device doesn’t bind:

 1. Confirm you injected the correct architecture (x86 vs x64).
 2. Confirm the device’s Hardware IDs match what the injected INF declares (`aero_virtio_snd.inf` vs `aero-virtio-snd-legacy.inf`) (Device Manager → Details → **Hardware Ids**).
3. Confirm signature policy didn’t block installation/loading (next section).

## Bring-up toggles (registry)

The virtio-snd INFs create per-device bring-up toggles with safe defaults (`0`) under the device instance registry key:

- `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend` (`REG_DWORD`)
  - `1`: force the silent null backend and allow `START_DEVICE` to succeed even when virtio transport bring-up fails
- `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\AllowPollingOnly` (`REG_DWORD`)
  - `1`: allow polling-only mode if no usable interrupt resource can be connected (neither MSI/MSI-X nor INTx)

Find `<DeviceInstancePath>` via **Device Manager → device → Details → “Device instance path”**.

After changing a toggle value, reboot the guest or disable/enable the device so Windows re-runs `START_DEVICE`.

Backwards compatibility note: older installs may have these values under the per-device driver key (the software key for the device/driver instance). The driver checks the per-device `Device Parameters` key first and falls back to the driver key.

---

## Driver signing / test-signing warnings

Windows 7’s kernel-mode driver signature policy can prevent unattended first-boot use:

- **Windows 7 x64** will not load unsigned kernel-mode drivers under normal boot policy.
- **Windows 7 x86** is more permissive, but may still prompt/warn depending on policy.

Important implications:

- `dism /Add-Driver /ForceUnsigned` can stage an unsigned driver into the image, but that does **not** guarantee Windows will install/load it at runtime.
- Common symptom on Win7 x64 when signature enforcement blocks a driver: **Code 52** (“Windows cannot verify the digital signature…”).

For automated / unattended first boot you generally want one of:

- a properly signed driver package, or
- a test-signed package **plus**:
  - test-signing enabled in the guest boot configuration, and
  - the signing certificate installed into the guest’s trusted roots/publishers.

If you’re experimenting on an already-booted machine, you can enable test signing and reboot:

```bat
bcdedit /set testsigning on
shutdown /r /t 0
```

For images that must “just work” on first boot, you’ll typically want to set `testsigning` offline in the image BCD (or `BCD-Template`) and inject the needed certificates offline.

See:

- `drivers/windows7/virtio-snd/README.md` (test cert + signing workflow)
- `docs/16-win7-image-servicing.md` (end-to-end servicing notes)
- `docs/win7-bcd-offline-patching.md` (offline BCD edits for test-signing / nointegritychecks)

---

## Optional: inject into `boot.wim` (WinPE/Setup)

`install.wim` injection makes the driver available in the installed OS. If you also want the driver staged in the Windows Setup environment (WinPE), inject the same driver into `boot.wim` **index 2**:

```bat
set BOOTWIM=C:\win7\sources\boot.wim
set BOOTMOUNT=C:\wim\boot-mount
mkdir %BOOTMOUNT%

REM Ensure VIRTIO_SND_INF_DIR points at the correct arch package.
dism /Mount-Wim /WimFile:%BOOTWIM% /Index:2 /MountDir:%BOOTMOUNT%
dism /Image:%BOOTMOUNT% /Add-Driver /Driver:%VIRTIO_SND_INF_DIR% /Recurse
dism /Unmount-Wim /MountDir:%BOOTMOUNT% /Commit
```

Most installs don’t need an audio driver during setup, but this can be useful if you’re validating driver staging in WinPE.
