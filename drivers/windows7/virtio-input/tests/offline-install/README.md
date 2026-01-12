# Offline / slipstream install: virtio-input driver into Windows 7 (WIM or offline OS)

This document describes how to **stage** (preinstall) the `virtio-input` driver into a
Windows 7 image so that Plug‑and‑Play can bind it **on first boot** (useful for automated
test images where you want input working immediately).

> Note: The in-tree Aero Win7 virtio-input INF is **revision-gated** to the
> `AERO-W7-VIRTIO` v1 contract and matches only `PCI\VEN_1AF4&DEV_1052&REV_01` (plus
> the more specific `...&SUBSYS_...&REV_01` variants). Ensure your virtio-input PCI
> device reports `REV_01` (for example in QEMU: `-device virtio-*-pci,...,x-pci-revision=0x01`)
> or Windows will not bind the staged driver.

The commands below assume you already have a **built driver package directory** containing:

- `aero_virtio_input.inf`
- `aero_virtio_input.sys`
- `aero_virtio_input.cat` (recommended for Win7 x64 unless you plan to use `/ForceUnsigned`)

In this repo, CI produces signed packages under:

```
out/packages/windows7/virtio-input/<arch>/
```

Where `<arch>` is `x86` or `x64` (CI output naming; DISM itself does not care about the folder name).

Point DISM at the directory (or `.inf`) produced by your build/sign pipeline.

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
- These steps are for `install.wim` (the installed OS). If you also need the driver available inside Windows Setup/WinPE, see the note in [Optional: boot.wim (WinPE) injection](#optional-bootwim-winpe-injection).

---

## Choose the correct driver (x86 vs x64)

You must inject a driver matching the target Windows 7 architecture:

- **Win7 x86 (32-bit)** → use the x86 package dir (e.g. `out\\packages\\windows7\\virtio-input\\x86\\`)
- **Win7 x64 (64-bit)** → use the x64 package dir (e.g. `out\\packages\\windows7\\virtio-input\\x64\\`)

In all cases, DISM’s `/Driver:` should ultimately point at a folder (or file) that contains the correct `.inf` for the image you’re servicing.

---

## Option A: Slipstream into Windows 7 install media (`install.wim`)

### 1) Identify which image index you will install

`install.wim` typically contains multiple editions. List them:

```bat
set WIM=C:\\win7\\sources\\install.wim
dism /Get-WimInfo /WimFile:%WIM%
```

Pick the `Index` for the edition you will actually install (e.g. “Windows 7 PROFESSIONAL”).

### 2) Mount the WIM

```bat
set MOUNT=C:\\wim\\mount
mkdir %MOUNT%

REM Example: mount index 1. Replace 1 with the index you chose.
dism /Mount-Wim /WimFile:%WIM% /Index:1 /MountDir:%MOUNT%
```

### 3) Add the virtio-input driver

Assuming this repo is checked out at `C:\\src\\aero` and you already ran the driver CI pipeline locally:

```bat
set REPO=C:\\src\\aero
set VIRTIO_INPUT_PKG=%REPO%\\out\\packages\\windows7\\virtio-input\\x64

dism /Image:%MOUNT% /Add-Driver /Driver:%VIRTIO_INPUT_PKG% /Recurse
```

If DISM rejects the package due to signature issues and you’re doing test-only images, you can try:

```bat
dism /Image:%MOUNT% /Add-Driver /Driver:%VIRTIO_INPUT_PKG% /Recurse /ForceUnsigned
```

See [Driver signing / test signing warnings](#driver-signing--test-signing-warnings) before relying on `/ForceUnsigned`.

### 4) Verify the driver is staged in the offline DriverStore

List 3rd-party drivers in the mounted image:

```bat
dism /Image:%MOUNT% /Get-Drivers /Format:Table
```

Then locate the `oem#.inf` entry corresponding to virtio-input and inspect it:

```bat
dism /Image:%MOUNT% /Get-DriverInfo /Driver:oem#.inf
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
- If you serviced a copied `install.wim`, copy it back into `...\\sources\\install.wim` before creating bootable media.

On first boot of the installed OS, Windows will enumerate the virtio-input hardware and should automatically select the best matching driver from the DriverStore (as long as the device reports `REV_01` so it matches the INF).

---

## Option B: Inject into an already-installed *offline* Windows directory (mounted VHD/VHDX)

This is useful if you already have a Windows 7 VM disk image and want the driver present on next boot without reinstalling.

1) Mount the VM’s system disk so it shows up as a drive letter (example uses DiskPart; you can also use Disk Management GUI).

For VHD/VHDX on Windows 10/11:

```bat
diskpart
DISKPART> select vdisk file="C:\\vm\\win7.vhd"
DISKPART> attach vdisk
DISKPART> list volume
DISKPART> exit
```

Identify the volume letter that contains `\\Windows\\` (example uses `W:`).

2) Add the driver to the offline Windows installation:

```bat
set OFFLINE=W:\\
set REPO=C:\\src\\aero
set VIRTIO_INPUT_PKG=%REPO%\\out\\packages\\windows7\\virtio-input\\x64

dism /Image:%OFFLINE% /Add-Driver /Driver:%VIRTIO_INPUT_PKG% /Recurse
```

3) Verify it’s staged:

```bat
dism /Image:%OFFLINE% /Get-Drivers /Format:Table
```

4) Detach the disk when done:

```bat
diskpart
DISKPART> select vdisk file="C:\\vm\\win7.vhd"
DISKPART> detach vdisk
DISKPART> exit
```

On the next boot of that VM, PnP should bind the device to the staged driver.

---

## Driver signing / test signing warnings

Windows 7’s kernel-mode driver signature policy can prevent unattended first-boot use:

- **x64 Windows 7** will not load unsigned kernel-mode drivers under normal boot policy.
- **x86 Windows 7** is more permissive, but may still prompt/warn depending on policy.

Important implications:

- `dism /Add-Driver /ForceUnsigned` can stage an unsigned driver into the image, but that does **not** guarantee Windows will load it at runtime.
- For automated / unattended first boot, you generally want a **properly signed** driver package (or you must arrange for test-signing / signature enforcement changes *before* the driver needs to load).

If you plan to use a test-signed build for automation, ensure your boot configuration and policies allow it (for example by enabling test signing in the image) and validate that your CI harness boots with the expected settings.

---

## Optional: `boot.wim` (WinPE) injection

`install.wim` injection makes the driver available in the installed OS. If you also need virtio-input working during the Windows Setup UI (WinPE), inject the same driver into `boot.wim` index 2 (the actual Setup environment):

```bat
set BOOTWIM=C:\\win7\\sources\\boot.wim
set MOUNT=C:\\wim\\boot-mount
mkdir %MOUNT%

dism /Mount-Wim /WimFile:%BOOTWIM% /Index:2 /MountDir:%MOUNT%
dism /Image:%MOUNT% /Add-Driver /Driver:%VIRTIO_INPUT_PKG% /Recurse
dism /Unmount-Wim /MountDir:%MOUNT% /Commit
```

This is optional for “first boot driver availability”, but helpful if your setup is fully automated and depends on virtio-input during installation.
