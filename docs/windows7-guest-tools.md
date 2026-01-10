# Windows 7 Guest Tools (Aero)

This guide walks you through installing Windows 7 in Aero using the **baseline (fully emulated)** device profile first, then installing **Aero Guest Tools** to enable the **paravirtual (virtio + Aero GPU)** drivers.

> Aero does **not** include or distribute Windows. You must provide your own Windows 7 ISO and license.

## Quick start (overview)

1. Install Windows 7 SP1 using **baseline devices**: **AHCI + e1000 + VGA**.
2. Mount `aero-guest-tools.iso` and run `setup.cmd` as Administrator.
3. Reboot once (still on baseline devices).
4. Switch devices in this order (reboot between each):
   1. **AHCI → virtio-blk**
   2. **e1000 → virtio-net**
   3. **VGA → Aero GPU**
5. Run `verify.cmd` as Administrator and check `report.txt`.

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

### Supported Windows 7 ISOs / editions

Aero Guest Tools is intended for **Windows 7 SP1**:

- ✅ Windows 7 **SP1 x86** and **SP1 x64**
- ✅ Most editions should work (Home Premium / Professional / Ultimate / Enterprise), as long as your license matches the media.
- ❌ Windows 7 **RTM (no SP1)** is not recommended (higher chance of installer/driver/update failures).

### SHA-256 / KB3033929 note (important for x64)

If Aero’s driver packages are signed with **SHA-256**, Windows 7 requires **KB3033929** to validate those signatures.

- If KB3033929 is missing you may see **Device Manager → Code 52** (“Windows cannot verify the digital signature…”).
- You can install KB3033929 after Windows is installed, or **slipstream** it into your ISO (see the optional section at the end).

### x86 vs x64 notes (driver signing and memory)

- **Windows 7 x86**
  - Does not enforce kernel-mode signature checks as strictly as x64, but you can still see warnings during driver install.
  - Practical benefit: can be easier to get started if you are troubleshooting signing issues.
- **Windows 7 x64**
  - Enforces kernel driver signature validation.
  - Depending on how the Aero drivers are signed, you may need:
    - the Aero signing certificate installed (via Guest Tools), and/or
    - **Test Signing** enabled, and/or
    - **KB3033929** if the catalogs are SHA-256.

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

### Optional (recommended for x64): install KB3033929 before Guest Tools

If you expect to use **SHA-256-signed** driver packages, install **KB3033929** while you are still on baseline devices (AHCI/e1000/VGA). This avoids confusing “unsigned driver” failures later.

See: [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md#issue-missing-kb3033929-sha-256-signature-support)

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

### x64: “Test Mode” is expected if test signing is enabled

If Guest Tools enables test signing on Windows 7 x64, you may see a desktop watermark like:

- `Test Mode Windows 7 ...`

This is normal for test-signed drivers. Only disable test signing after you have confirmed you are using production-signed drivers (see the troubleshooting guide).

## What `setup.cmd` changes

The exact actions depend on the Guest Tools version, but the workflow generally includes:

### 1) Certificate store

Installs Aero’s driver signing certificate into the **Local Machine** certificate stores so Windows can trust Aero’s driver packages. Common targets:

- **Trusted Root Certification Authorities**
- **Trusted Publishers**

### 2) Boot configuration (BCD)

May update the boot configuration database via `bcdedit`, for example:

- Enabling **Test Signing** (`testsigning on`) so test-signed kernel drivers load on Windows 7 x64.
- Optionally increasing Boot Manager timeouts so recovery options are easier to reach in an emulator (for example, setting a non-zero `timeout`).

### 3) Driver store / PnP staging

Stages the Aero drivers into the Windows driver store (so that when you later switch devices, Windows can bind the correct drivers automatically). Common mechanisms include:

- `pnputil -i -a <driver.inf>`
- `dism /online /add-driver /driver:<path> /recurse`

### 4) Registry / service configuration

Configures driver services and boot-critical settings (especially important for storage drivers), for example:

- Ensuring the **virtio storage** driver is set to start at boot when needed.
- Setting device/service parameters under `HKLM\SYSTEM\CurrentControlSet\Services\...`

## If `setup.cmd` fails: manual install (advanced)

If `setup.cmd` fails (or you prefer to install components manually), you can typically do the same work yourself.

> The exact file names and folder layout inside `aero-guest-tools.iso` may vary by version. The commands below use placeholders.

### 1) Import the Aero signing certificate (Local Machine)

Look on the Guest Tools media for a certificate file (commonly `.cer` / `.crt`).

From an elevated Command Prompt:

- `certutil -addstore -f Root X:\path\to\aero.cer`
- `certutil -addstore -f TrustedPublisher X:\path\to\aero.cer`

(`X:` is usually the Guest Tools CD drive letter.)

### 2) Enable test signing (Windows 7 x64, if required)

From an elevated Command Prompt:

- `bcdedit /set {current} testsigning on`
- Reboot

If you are using production-signed drivers, keep test signing off.

### 3) Stage/install drivers into the driver store

Use either `pnputil` (Windows 7 built-in) or DISM:

- `pnputil -i -a X:\path\to\drivers\*.inf`
- or: `dism /online /add-driver /driver:X:\path\to\drivers\ /recurse`

After staging, reboot once while still on baseline devices.

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

If the virtio-net driver fails to bind and you lose networking, you can always switch back to **e1000** to regain connectivity while troubleshooting.

### Stage C: switch graphics (VGA → Aero GPU)

1. Shut down Windows.
2. Switch graphics from **VGA** to **Aero GPU**.
3. Boot Windows.

Expected behavior:

- The screen may flicker as Windows binds the new display driver.
- You should be able to set higher resolutions.
- To enable the Aero Glass theme, you may need to select a Windows 7 theme in **Personalization** and/or run **Performance Information and Tools** once.
  - If “Aero” themes are unavailable, running `winsat formal` (from an elevated Command Prompt) often enables them after reboot.

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

1. Copy ISO contents to a working folder (example: `C:\win7-iso\`).
2. Identify your `install.wim` index:
   - `dism /Get-WimInfo /WimFile:C:\win7-iso\sources\install.wim`
3. Mount `install.wim` (example index `1`):
   - `mkdir C:\wim\mount`
   - `dism /Mount-Wim /WimFile:C:\win7-iso\sources\install.wim /Index:1 /MountDir:C:\wim\mount`
4. Add KB3033929:
   - `dism /Image:C:\wim\mount /Add-Package /PackagePath:C:\updates\KB3033929-x64.msu`
   - If DISM refuses the `.msu`, extract it first and add the `.cab` instead:
     - `expand -F:* C:\updates\KB3033929-x64.msu C:\updates\KB3033929\`
     - `dism /Image:C:\wim\mount /Add-Package /PackagePath:C:\updates\KB3033929\Windows6.1-KB3033929-x64.cab`
5. Add Aero drivers:
   - `dism /Image:C:\wim\mount /Add-Driver /Driver:C:\drivers\aero\ /Recurse`
6. Commit and unmount:
   - `dism /Unmount-Wim /MountDir:C:\wim\mount /Commit`

If you want Windows Setup itself to see a virtio-blk disk during installation, you must also add the storage driver to `sources\\boot.wim` (indexes 1 and 2).

Example (adding drivers to `boot.wim`):

1. Check boot indexes:
   - `dism /Get-WimInfo /WimFile:C:\win7-iso\sources\boot.wim`
2. Mount index 1 and add drivers:
   - `dism /Mount-Wim /WimFile:C:\win7-iso\sources\boot.wim /Index:1 /MountDir:C:\wim\mount`
   - `dism /Image:C:\wim\mount /Add-Driver /Driver:C:\drivers\aero\ /Recurse`
   - `dism /Unmount-Wim /MountDir:C:\wim\mount /Commit`
3. Repeat for index 2.

### Rebuilding a bootable ISO (optional)

After modifying the WIM(s), you must rebuild a bootable ISO from your working folder. A common approach on Windows is `oscdimg` (Windows ADK):

- `oscdimg -m -o -u2 -udfver102 -bootdata:2#p0,e,bC:\win7-iso\boot\etfsboot.com#pEF,e,bC:\win7-iso\efi\microsoft\boot\efisys.bin C:\win7-iso C:\win7-slipstream.iso`

If your source ISO does not contain `efi\microsoft\boot\efisys.bin`, omit the UEFI boot entry.

For detailed recovery and switch-over pitfalls, see the troubleshooting guide:

- [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md)
