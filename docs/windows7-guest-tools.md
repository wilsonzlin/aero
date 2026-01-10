# Windows 7 Guest Tools (Aero)

This guide walks you through installing Windows 7 in Aero using the **baseline (fully emulated)** device profile first, then installing **Aero Guest Tools** to enable the **paravirtual (virtio + Aero GPU)** drivers.

> Aero does **not** include or distribute Windows. You must provide your own Windows 7 ISO and license.

## Prerequisites

### What you need

- A Windows 7 **SP1** ISO for **x86 (32-bit)** or **x64 (64-bit)**.
  - Recommended: official Microsoft/MSDN/OEM media (unmodified).
  - Avoid “pre-activated”, “all-in-one”, or heavily modified ISOs (they frequently break servicing, signing, or boot).
- A Windows 7 product key/license that matches your ISO edition.
- `aero-guest-tools.iso` (shipped with Aero releases/builds).
- Enough resources for the guest:
  - Disk: **30–40 GB** recommended for a comfortable Win7 install.
  - Memory: **2 GB** minimum (x86), **3–4 GB** recommended (x64).

### Why Windows 7 SP1 matters

Windows 7 RTM is missing years of fixes. SP1 significantly reduces installer and driver friction.

### SHA-256 / KB3033929 note (important for x64)

If Aero’s driver packages are signed with **SHA-256**, Windows 7 requires **KB3033929** to validate those signatures.

- If KB3033929 is missing you may see **Device Manager → Code 52** (“Windows cannot verify the digital signature…”).
- You can install KB3033929 after Windows is installed, or **slipstream** it into your ISO (see the optional section at the end).

## Step 1: Install Windows 7 using baseline (compatibility) devices

Install Windows first using the baseline emulated devices (this avoids needing any third-party drivers during Windows Setup).

In Aero, create a new Windows 7 VM/guest and select the **baseline / compatibility** profile:

- **Storage:** SATA **AHCI**
- **Network:** Intel **e1000**
- **Graphics:** **VGA**
- **Input:** PS/2 keyboard + mouse (or Aero’s default input devices)

Then:

1. Attach your Windows 7 ISO as the virtual CD/DVD.
2. Boot the VM and complete Windows Setup normally.
3. Confirm you can reach a stable desktop.

Recommended (but optional): take a snapshot/checkpoint here if your host environment supports it.

## Step 2: Mount `aero-guest-tools.iso`

After you have a working Windows 7 desktop:

1. Eject/unmount the Windows installer ISO.
2. Mount/insert `aero-guest-tools.iso` as the virtual CD/DVD.
3. In Windows 7, open **Computer** and verify you see the CD drive.

### Recommended: copy Guest Tools to a writable folder

Some scripts generate logs/reports. To avoid write-permission issues when running from read-only media, copy the ISO contents to a local folder:

1. Create a folder like `C:\AeroGuestTools\`
2. Copy all files from the Guest Tools CD into `C:\AeroGuestTools\`

## Step 3: Run `setup.cmd` as Administrator

1. Navigate to `C:\AeroGuestTools\` (or the mounted CD if you didn’t copy it).
2. Right-click `setup.cmd` → **Run as administrator**.
3. Accept any UAC prompts.

During installation you may see driver install prompts:

- **Windows 7 x86:** Windows may warn about unsigned drivers. Choose **Install this driver software anyway** (only if you trust the Guest Tools you’re using).
- **Windows 7 x64:** Windows enforces kernel driver signatures. Guest Tools will typically enable **test signing** (see below) and install Aero’s signing certificate so the drivers can load.

When `setup.cmd` finishes, reboot Windows if prompted.

## What `setup.cmd` changes

The exact actions depend on the Guest Tools version, but the workflow generally includes:

### 1) Certificate store

Installs Aero’s driver signing certificate into the **Local Machine** certificate stores so Windows can trust Aero’s driver packages. Common targets:

- **Trusted Root Certification Authorities**
- **Trusted Publishers**

### 2) Boot configuration (BCD)

May update the boot configuration database via `bcdedit`, for example:

- Enabling **Test Signing** (`testsigning on`) so test-signed kernel drivers load on Windows 7 x64.
- Optionally setting a legacy boot menu policy to make **F8 / Safe Mode** easier to access.

### 3) Driver store / PnP staging

Stages the Aero drivers into the Windows driver store (so that when you later switch devices, Windows can bind the correct drivers automatically). Common mechanisms include:

- `pnputil -i -a <driver.inf>`
- `dism /online /add-driver /driver:<path> /recurse`

### 4) Registry / service configuration

Configures driver services and boot-critical settings (especially important for storage drivers), for example:

- Ensuring the **virtio storage** driver is set to start at boot when needed.
- Setting device/service parameters under `HKLM\SYSTEM\CurrentControlSet\Services\...`

## Step 4: Reboot (still on baseline devices)

After running Guest Tools, reboot once while still using baseline devices. This confirms the OS still boots normally before changing storage/network/display hardware.

## Step 5: Switch to virtio + Aero GPU (recommended order)

To reduce the chance of an unrecoverable boot issue, switch devices **in stages** and verify Windows boots between each step.

### Stage A: switch storage (AHCI → virtio-blk)

1. Shut down Windows cleanly.
2. In Aero’s VM settings, switch the **system disk controller** from **AHCI** to **virtio-blk**.
3. Boot Windows.

Expected behavior:

- Windows boots to desktop.
- It may install new devices and ask for another reboot.

### Stage B: switch networking (e1000 → virtio-net)

1. Shut down Windows.
2. Switch the network adapter from **e1000** to **virtio-net**.
3. Boot Windows and confirm networking works (optional).

### Stage C: switch graphics (VGA → Aero GPU)

1. Shut down Windows.
2. Switch graphics from **VGA** to **Aero GPU**.
3. Boot Windows.

Expected behavior:

- The screen may flicker as Windows binds the new display driver.
- You should be able to set higher resolutions.
- To enable the Aero Glass theme, you may need to select a Windows 7 theme in **Personalization** and/or run **Performance Information and Tools** once.

## Step 6: Run `verify.cmd` and interpret `report.txt`

After you can boot with virtio + Aero GPU:

1. Open `C:\AeroGuestTools\`
2. Right-click `verify.cmd` → **Run as administrator**
3. Open the generated `report.txt`

Typical things `verify.cmd` reports:

- OS version and architecture (x86 vs x64)
- Whether **Test Signing** is enabled (important on x64 if drivers are test-signed)
- Whether the Aero certificate is present in the expected certificate stores
- Device/driver binding status for:
  - virtio-blk storage
  - virtio-net networking
  - Aero GPU graphics

If `report.txt` shows failures or warnings, see:

- [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md)

## Safe rollback path (if virtio-blk boot fails)

If Windows fails to boot after switching to **virtio-blk** (common symptoms: boot loop or a BSOD like `0x0000007B INACCESSIBLE_BOOT_DEVICE`):

1. Power off the VM.
2. Switch the disk controller back to **AHCI** in Aero’s VM settings.
3. Boot Windows.
4. Re-run `setup.cmd` as Administrator and reboot once more.
5. Try switching to virtio-blk again (and avoid changing multiple device classes at once).

## Optional: Slipstream KB3033929 and/or drivers into your Windows 7 ISO

Slipstreaming is optional, but can reduce first-boot driver/signature problems (especially for offline installs).

**Rules:**

- Only modify ISOs you legally own.
- Do not redistribute the resulting ISO.

### What you can slipstream

- **KB3033929** (SHA-256 signature support)
- Aero driver `.inf` packages (virtio-blk/net and optionally Aero GPU)

### High-level DISM approach (Windows host)

On a Windows 10/11 host (or a Windows VM), you can use DISM:

1. Copy ISO contents to a working folder.
2. Mount `sources\\install.wim` for the edition you plan to install.
3. `dism /image:<mount> /add-package` for KB3033929.
4. `dism /image:<mount> /add-driver /driver:<path> /recurse` to add drivers.
5. Commit/unmount the WIM.

If you want Windows Setup itself to see a virtio-blk disk during installation, you must also add the storage driver to `sources\\boot.wim` (indexes 1 and 2).

For detailed recovery and switch-over pitfalls, see the troubleshooting guide:

- [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md)

