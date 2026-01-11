# Offline / slipstream install: virtio-snd driver into Windows 7 (WIM or offline OS)

This document describes how to **stage** (preinstall) the `virtio-snd` driver into a Windows 7 image so that Plug‑and‑Play can bind it **on first boot** (useful for automated test images where you want audio working immediately).

The driver artifacts referenced here are built/staged in this repo under:

```
drivers/windows7/virtio-snd/inf/
```

Point DISM at the directory (or `.inf`) produced by your build. In a “ready to inject” package, that directory should contain:

- `virtio-snd.inf`
- `virtiosnd.sys` (architecture-specific)
- `virtio-snd.cat` (optional but strongly recommended; required for signature enforcement scenarios)

> Note: `virtio-snd` is **not boot-critical** (it’s a PnP media device, StartType=3). If staging fails or the driver is blocked by signature policy at runtime, **Windows will still boot**; you’ll just have missing audio / an unknown device to troubleshoot.

---

## Prerequisites

- A Windows host with `dism.exe` available:
  - Windows 7 (built-in DISM), or
  - Windows 10/11 (built-in DISM; generally works fine for servicing Win7 WIMs).
- An **elevated** Command Prompt (`Run as administrator`).
- Writable working directory with enough free space (mounting a WIM expands it).
- Windows 7 install media contents available on disk (USB folder or extracted ISO).

Notes:
- If your install media is mounted as a read-only ISO, **copy `install.wim` to a writable folder** before servicing.
- These steps are for `install.wim` (the installed OS). If you also need the driver available inside Windows Setup/WinPE, see [Optional: boot.wim (WinPE) injection](#optional-bootwim-winpe-injection).

---

## Choose the correct driver (x86 vs x64)

You must inject a driver matching the target Windows 7 architecture:

- **Win7 x86 (32-bit)** → use an x86 build of `virtiosnd.sys`
- **Win7 x64 (64-bit)** → use an amd64/x64 build of `virtiosnd.sys`

Because both architectures use the same filenames (`virtiosnd.sys`, `virtio-snd.cat`), it’s easiest to keep two separate package directories (example only):

```
C:\drivers\virtio-snd\x86\virtio-snd.inf
C:\drivers\virtio-snd\x86\virtiosnd.sys
C:\drivers\virtio-snd\x86\virtio-snd.cat

C:\drivers\virtio-snd\amd64\virtio-snd.inf
C:\drivers\virtio-snd\amd64\virtiosnd.sys
C:\drivers\virtio-snd\amd64\virtio-snd.cat
```

In all cases, DISM’s `/Driver:` should ultimately point at a folder (or file) that contains the correct `virtio-snd.inf` for the image you’re servicing.

---

## Option A: Slipstream into Windows 7 install media (`install.wim`)

### 1) Identify which image index you will install

`install.wim` typically contains multiple editions. List them:

```bat
set WIM=C:\win7\sources\install.wim
dism /Get-WimInfo /WimFile:%WIM%
```

Pick the `Index` for the edition you will actually install (e.g. “Windows 7 PROFESSIONAL”).

### 2) Mount the WIM

```bat
set MOUNT=C:\wim\mount
mkdir %MOUNT%

REM Example: mount index 1. Replace 1 with the index you chose.
dism /Mount-Wim /WimFile:%WIM% /Index:1 /MountDir:%MOUNT%
```

### 3) Add the virtio-snd driver

Assuming this repo is checked out at `C:\src\aero`:

```bat
set REPO=C:\src\aero

REM Point this at a folder containing virtio-snd.inf + virtiosnd.sys for the
REM correct architecture (x86 vs amd64).
set VIRTIO_SND_INF_DIR=%REPO%\drivers\windows7\virtio-snd\inf\

dism /Image:%MOUNT% /Add-Driver /Driver:%VIRTIO_SND_INF_DIR% /Recurse
```

If DISM rejects the package due to signature issues and you’re doing test-only images, you can try:

```bat
dism /Image:%MOUNT% /Add-Driver /Driver:%VIRTIO_SND_INF_DIR% /Recurse /ForceUnsigned
```

See [Driver signing / test signing warnings](#driver-signing--test-signing-warnings) before relying on `/ForceUnsigned`.

### 4) Verify the driver is staged in the offline DriverStore

List 3rd-party drivers in the mounted image:

```bat
dism /Image:%MOUNT% /Get-Drivers /Format:Table
```

Then locate the `oem#.inf` entry corresponding to virtio-snd and inspect it:

```bat
dism /Image:%MOUNT% /Get-DriverInfo /Driver:oem#.inf
```

Additional sanity check (optional): the driver package should be present under the mounted image’s DriverStore, for example:

```
%MOUNT%\Windows\System32\DriverStore\FileRepository\
```

### 5) Commit changes and unmount

DISM supports committing without unmounting (useful if you plan more modifications):

```bat
dism /Commit-Wim /MountDir:%MOUNT%
```

Then unmount:

```bat
dism /Unmount-Wim /MountDir:%MOUNT% /Commit
```

If something went wrong and you want to abandon changes:

```bat
dism /Unmount-Wim /MountDir:%MOUNT% /Discard
```

If DISM reports an “already mounted”/stale mount directory from a previous run, you may need:

```bat
dism /Cleanup-Wim
```

### 6) Use the updated `install.wim`

- If you serviced `install.wim` in place under your extracted install media folder, you’re done.
- If you serviced a copied `install.wim`, copy it back into `...\sources\install.wim` before creating bootable media.

On first boot of the installed OS, Windows will enumerate the virtio-snd hardware and should automatically select the best matching driver from the DriverStore.

---

## Option B: Inject into an already-installed *offline* Windows directory (mounted VHD/VHDX)

This is useful if you already have a Windows 7 VM disk image and want the driver present on next boot without reinstalling.

1) Mount the VM’s system disk so it shows up as a drive letter (example uses DiskPart; you can also use Disk Management GUI).

For VHD/VHDX on Windows 10/11:

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
set VIRTIO_SND_INF_DIR=%REPO%\drivers\windows7\virtio-snd\inf\

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

On the next boot of that VM, PnP should bind the device to the staged driver.

---

## Verification after first boot (inside Windows 7)

Once the system boots with virtio-snd hardware present:

- `devmgmt.msc` → verify **Aero VirtIO Sound Device** is present and is using the expected virtio-snd driver.
  - Typical category after install: **Sound, video and game controllers**
  - Driver Details should include `virtiosnd.sys`
- `pnputil -e` (lists staged driver packages) → verify the virtio-snd INF package exists.
- `%WINDIR%\inf\setupapi.dev.log` → search for the virtio-snd hardware ID (`PCI\VEN_1AF4&DEV_1059...`) or `virtio-snd.inf` and confirm it selected your INF and installed without prompting.

If the driver is staged but the device doesn’t bind:

1) Confirm you injected the correct architecture (x86 vs x64).
2) Confirm the INF actually matches the device’s Hardware IDs (Device Manager → device → Details → “Hardware Ids”).
3) Confirm signature policy didn’t block installation (see below).

---

## Driver signing / test signing warnings

Windows 7’s kernel-mode driver signature policy can prevent unattended first-boot use:

- **x64 Windows 7** will not load unsigned kernel-mode drivers under normal boot policy.
- **x86 Windows 7** is more permissive, but may still prompt/warn depending on policy.

Important implications:

- `dism /Add-Driver /ForceUnsigned` can stage an unsigned driver into the image, but that does **not** guarantee Windows will load it at runtime.
- For automated / unattended first boot, you generally want a **properly signed** driver package (or you must arrange for test-signing / signature enforcement changes *before* the driver needs to load).

If you plan to use a test-signed build for automation, ensure your boot configuration and policies allow it (for example by enabling test signing in the image) and validate that your CI harness boots with the expected settings.

Reminder: virtio-snd is non-boot-critical. A signature failure typically results in a Code 52 device error or an unknown PCI device, not a boot failure.

---

## Optional: `boot.wim` (WinPE) injection

`install.wim` injection makes the driver available in the installed OS. If you also need virtio-snd present inside the Windows Setup environment (WinPE), inject the same driver into `boot.wim` index 2 (the actual Setup environment):

```bat
set BOOTWIM=C:\win7\sources\boot.wim
set MOUNT=C:\wim\boot-mount
mkdir %MOUNT%

dism /Mount-Wim /WimFile:%BOOTWIM% /Index:2 /MountDir:%MOUNT%
dism /Image:%MOUNT% /Add-Driver /Driver:%VIRTIO_SND_INF_DIR% /Recurse
dism /Unmount-Wim /MountDir:%MOUNT% /Commit
```

This is optional for “first boot driver availability”; most setups do not need audio drivers during installation.
