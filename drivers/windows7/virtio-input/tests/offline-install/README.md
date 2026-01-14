# Offline / slipstream install: virtio-input driver into Windows 7 (WIM or offline OS)

This document describes how to **stage** (preinstall) the `virtio-input` driver into a
Windows 7 image so that Plug‑and‑Play can bind it **on first boot** (useful for automated
test images where you want input working immediately).

> Note: The in-tree Aero Win7 virtio-input INFs are **revision-gated** to the `AERO-W7-VIRTIO` v1 contract (`REV_01`).
> Ensure your virtio-input PCI device reports `REV_01` (for example in QEMU:
> `-device virtio-*-pci,...,x-pci-revision=0x01`) or Windows will not bind the staged driver.
>
> Driver packages:
>
> - Keyboard/mouse: `aero_virtio_input.inf`
>   - Contract keyboard HWID: `...&SUBSYS_00101AF4&REV_01`
>   - Contract mouse HWID: `...&SUBSYS_00111AF4&REV_01`
>   - Note: canonical INF is intentionally **SUBSYS-only** (no strict generic fallback).
> - Tablet/absolute pointer: `aero_virtio_tablet.inf` (`...&SUBSYS_00121AF4&REV_01`)
>   - Tablet binding is more specific, so it wins over the generic fallback when the tablet subsystem ID is present and
>     both packages are installed.
>   - If the tablet subsystem ID is missing (or the tablet INF is not staged), the device may bind via the generic fallback
>     entry **when enabled via the legacy alias INF** and show up as **Aero VirtIO Input Device**.
> - Optional legacy filename alias: `virtio-input.inf.disabled` → rename to `virtio-input.inf` to enable.
>   - Adds opt-in strict generic fallback binding (no `SUBSYS`): `PCI\VEN_1AF4&DEV_1052&REV_01`
>     - When binding via the fallback entry, Device Manager will show the generic **Aero VirtIO Input Device** name.
>   - Alias sync policy: outside the models sections (`[Aero.NTx86]` / `[Aero.NTamd64]`), from the first section header
>     (`[Version]`) onward, it must remain byte-identical to `aero_virtio_input.inf` (banner/comments may differ; see
>     `drivers/windows7/virtio-input/scripts/check-inf-alias.py`).
>   - Do **not** stage/install both basenames at once: choose **either** `aero_virtio_input.inf` **or** `virtio-input.inf`.

The commands below assume you already have a **built driver package directory** containing:

- `aero_virtio_input.inf` (keyboard + mouse)
- `aero_virtio_tablet.inf` (tablet / absolute pointer)
- `aero_virtio_input.sys`
- `aero_virtio_input.cat` (recommended for Win7 x64 unless you plan to use `/ForceUnsigned`)
- `aero_virtio_tablet.cat` (recommended for Win7 x64 unless you plan to use `/ForceUnsigned`)

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
- An **elevated** PowerShell / Command Prompt (`Run as administrator`).
- Writable working directory with enough free space (mounting a WIM expands it).
- Windows 7 install media contents available on disk (USB folder or extracted ISO).

Notes:

- If your install media is mounted as a read-only ISO, **copy `install.wim` to a writable folder** before servicing.
- These steps are for `install.wim` (the installed OS). If you also need the driver available inside Windows Setup/WinPE, see the note in [Optional: boot.wim (WinPE) injection](#optional-bootwim-winpe-injection).

---

## Quick start (scripted)

This directory includes a hardened DISM wrapper script:

- `inject-driver.ps1` (PowerShell; canonical entrypoint)
- `inject-driver.cmd` (thin CMD wrapper that forwards args to the PowerShell script)

The script supports both:

- **WIM mode**: mount one selected WIM index, inject the driver, verify, then unmount (commit by default)
- **Offline directory mode**: inject + verify directly into a mounted/offline Windows directory (e.g. a VHD attached as a drive letter)

Run from an **elevated** prompt.

### WIM mode: slipstream into `install.wim` (one index)

```powershell
powershell -ExecutionPolicy Bypass -File inject-driver.ps1 `
  -WimPath C:\win7\sources\install.wim `
  -Index 1 `
  -DriverDir C:\path\to\pkg\x64
```

Notes:

- Use `-Commit:$false` to mount/inject/verify but **discard** changes at the end (dry run).
- Use `-ForceUnsigned` only for test images. See [Driver signing / test signing warnings](#driver-signing--test-signing-warnings).

### Offline directory mode: inject into an offline Windows directory (mounted VHD/VHDX)

```powershell
# Example: W:\ is the offline Windows root and contains W:\Windows\
powershell -ExecutionPolicy Bypass -File inject-driver.ps1 `
  -OfflineDir W:\ `
  -DriverDir C:\path\to\pkg\x64
```

If the script fails and DISM reports a stale mount, try:

```bat
dism /Get-MountedWimInfo
dism /Cleanup-Wim
```

### Verification-only (CI-friendly)

To verify an offline image (mounted WIM dir or offline Windows dir) contains the staged virtio-input driver:

```bat
powershell -ExecutionPolicy Bypass -File Verify-VirtioInputStaged.ps1 -ImagePath W:\
echo %ERRORLEVEL%
```

### Legacy / deprecated script names

These scripts are kept for backward compatibility but are **deprecated**. Prefer
`inject-driver.ps1` / `inject-driver.cmd`.

- `Inject-VirtioInputDriver.ps1` (WIM mode wrapper): maps `-DriverPackageDir` → `-DriverDir`
- `Inject-VirtioInputDriverOffline.ps1` (offline directory wrapper): maps `-ImagePath` → `-OfflineDir` and
  `-DriverPackageDir` → `-DriverDir`

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

For a quick yes/no check (handy for CI), run the verifier script in this directory:

```bat
powershell -ExecutionPolicy Bypass -File Verify-VirtioInputStaged.ps1 -ImagePath %MOUNT%
echo %ERRORLEVEL%
```

It exits `0` when `aero_virtio_input.inf` is present in the offline DriverStore, and non-zero otherwise.

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

### Scripted injection

Use `inject-driver.ps1` in offline directory mode (see [Quick start (scripted)](#quick-start-scripted)).

`Inject-VirtioInputDriverOffline.ps1` is kept for backward compatibility but is **deprecated** and now
forwards to `inject-driver.ps1` (see “Legacy / deprecated script names” above).

### Manual DISM steps (for reference)

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

Or use the quick verifier script:

```bat
powershell -ExecutionPolicy Bypass -File Verify-VirtioInputStaged.ps1 -ImagePath %OFFLINE%
echo %ERRORLEVEL%
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
