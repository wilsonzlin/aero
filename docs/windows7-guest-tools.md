# Windows 7 Guest Tools (Aero)

This guide walks you through installing Windows 7 in Aero using the **baseline (fully emulated)** device profile first, then installing **Aero Guest Tools** to enable the **paravirtual (virtio + Aero GPU)** drivers.

> Aero does **not** include or distribute Windows. You must provide your own Windows 7 ISO and license.

If you are building from source / working on a PR, the GitHub Actions workflow
`.github/workflows/drivers-win7.yml` uploads an `aero-guest-tools` artifact containing:

- `aero-guest-tools.iso`
- `aero-guest-tools.zip`
- `manifest.json` (build metadata + SHA-256 hashes)
- `aero-guest-tools.manifest.json` (alias of `manifest.json` used by CI/release asset publishing)

## Quick start (overview)

1. Install Windows 7 SP1 using **baseline devices**: **AHCI (HDD) + IDE/ATAPI (CD-ROM) + e1000 + VGA**.
   - For the canonical Win7 install topology (AHCI HDD + IDE/ATAPI CD-ROM), see
     [`docs/05-storage-topology-win7.md`](./05-storage-topology-win7.md).
2. Mount `aero-guest-tools.iso` and run `setup.cmd` as Administrator.
3. Reboot once (still on baseline devices).
   - If you used `setup.cmd /skipstorage` (GPU-only / partial Guest Tools media), keep the boot disk on **AHCI** and skip step **4.1** (AHCI → virtio-blk). You can still switch other devices.
4. Switch devices in this order (reboot between each):
   1. **AHCI → virtio-blk**
   2. **e1000 → virtio-net**
   3. **VGA → Aero GPU**
   4. (Optional) **PS/2 → virtio-input**
   5. (Optional) **HDA → virtio-snd**
5. Run `verify.cmd` as Administrator and check `report.txt`.

> Note: Aero’s Windows 7 virtio device contract (`AERO-W7-VIRTIO` v1) encodes the contract major version in the PCI
> Revision ID (`REV_01`). Aero’s in-tree Win7 virtio driver packages are revision-gated (`&REV_01`), and some drivers also validate
> the revision at runtime, so if you are testing under QEMU or another
> VMM you may need to set `x-pci-revision=0x01` on the virtio devices (and preferably `disable-legacy=on`) for the
> drivers to bind.

If you are validating **virtio-input** specifically (device model + Win7 driver + web runtime routing), see:

- [`virtio-input-test-plan.md`](./virtio-input-test-plan.md)

## Contents

- [Prerequisites](#prerequisites)
- [Step 1: Install Windows 7 using baseline devices](#step-1-install-windows-7-using-baseline-compatibility-devices)
- [Step 2: Mount `aero-guest-tools.iso`](#step-2-mount-aero-guest-toolsiso)
- [Step 3: Run `setup.cmd` as Administrator](#step-3-run-setupcmd-as-administrator)
- [What `setup.cmd` changes](#what-setupcmd-changes)
- [If `setup.cmd` fails: manual install](#if-setupcmd-fails-manual-install-advanced)
- [Step 4: Reboot (still on baseline devices)](#step-4-reboot-still-on-baseline-devices)
- [Step 5: Switch to virtio + Aero GPU](#step-5-switch-to-virtio--aero-gpu-recommended-order)
- [Step 6: Run `verify.cmd` / read `report.txt`](#step-6-run-verifycmd-and-interpret-reporttxt)
- [Rollback paths](#safe-rollback-path-if-virtio-blk-boot-fails)
- [Optional: uninstall Guest Tools](#optional-uninstall-guest-tools)
- [Optional: slipstream SHA-2 updates and drivers](#optional-slipstream-sha-2-updates-and-drivers-into-your-windows-7-iso)
- [Troubleshooting](./windows7-driver-troubleshooting.md)

## Prerequisites

### What you need

- A Windows 7 **SP1** ISO for **x86 (32-bit)** or **x64 (64-bit)**.
  - Recommended: official Microsoft/MSDN/OEM media (unmodified).
  - Avoid “pre-activated”, “all-in-one”, or heavily modified ISOs (they frequently break servicing, signing, or boot).
- A Windows 7 product key/license that matches your ISO edition.
- `aero-guest-tools.iso` (or `aero-guest-tools.zip`) shipped with Aero builds/releases.
  - These artifacts are produced by CI from **signed driver packages** and include a `manifest.json` for integrity/version reporting.
  - If you're building from source/CI, see [`docs/16-guest-tools-packaging.md`](./16-guest-tools-packaging.md) for how the ISO/zip is produced from signed driver packages.
- Enough resources for the guest:
  - Disk: **30–40 GB** recommended for a comfortable Win7 install.
  - Memory: **2 GB** minimum (x86), **3–4 GB** recommended (x64).

### Device profiles used in this guide

This guide intentionally starts with **baseline (fully emulated)** devices for maximum installer compatibility, then switches to **paravirtual** devices for performance after Guest Tools is installed.

The exact names vary by Aero version/UI, but the mapping is typically:

| Subsystem | Baseline (install/recovery) | Performance (after Guest Tools) | Notes |
| --- | --- | --- | --- |
| Storage | AHCI (SATA) | virtio-blk | Switch this first; easiest to brick boot if done too early. |
| Network | Intel e1000 | virtio-net | If networking breaks, switch back to e1000 and boot. |
| Graphics | VGA | Aero GPU | If you get a black screen, switch back to VGA and recover. |
| Input | PS/2 keyboard + mouse | virtio-input | Optional; switch last. If input breaks, switch back to PS/2. |
| Audio | HDA (Intel HD Audio) | virtio-snd | Optional; does not affect boot. If audio breaks, keep HDA. For baseline HDA validation, see [`docs/testing/audio-windows7.md`](./testing/audio-windows7.md). |

### Why Windows 7 SP1 matters

Windows 7 RTM is missing years of fixes. SP1 significantly reduces installer and driver friction.

### Supported Windows 7 ISOs / editions

Aero Guest Tools is intended for **Windows 7 SP1**:

- ✅ Windows 7 **SP1 x86** and **SP1 x64**
- ✅ Most editions should work (Home Premium / Professional / Ultimate / Enterprise), as long as your license matches the media.
- ❌ Windows 7 **RTM (no SP1)** is not recommended (higher chance of installer/driver/update failures).

### SHA-256 / SHA-2 updates note (important for x64)

If Aero’s driver packages (or signing certificates) use **SHA-256 / SHA-2**, stock Windows 7 SP1 may require SHA-2-related updates such as **KB3033929** (and sometimes also **KB4474419**) to validate driver signatures.

- If the required SHA-2 updates are missing you may see **Device Manager → Code 52** (“Windows cannot verify the digital signature…”).
- You can install the required updates after Windows is installed, or **slipstream** them into your ISO (see the optional section at the end).

### x86 vs x64 notes (driver signing and memory)

- **Windows 7 x86**
  - Does not enforce kernel-mode signature checks as strictly as x64, but you can still see warnings during driver install.
  - Practical benefit: can be easier to get started if you are troubleshooting signing issues.
- **Windows 7 x64**
  - Enforces kernel driver signature validation.
  - Aero Guest Tools media includes a `manifest.json` that describes signing expectations via `signing_policy`:
    - `test`: Guest Tools will install certificate(s) (from `certs\`) and may prompt to enable **Test Signing**.
    - `production`: drivers are production/WHQL-signed; no custom certificate or Test Signing is expected.
    - `none`: no signing expectations (development use).
  - You may still need SHA-2 updates (commonly **KB3033929**, sometimes **KB4474419**) if the driver catalogs/certificates are SHA-2-signed.

## Step 1: Install Windows 7 using baseline (compatibility) devices

Install Windows first using the baseline emulated devices (this avoids needing any third-party drivers during Windows Setup).

In Aero, create a new Windows 7 VM/guest and select the **baseline / compatibility** profile:

- **Storage:** SATA **AHCI**
  - The Windows installer ISO is typically exposed as an **ATAPI CD-ROM** on a **PIIX3 IDE**
    controller in the canonical Win7 topology; see [`docs/05-storage-topology-win7.md`](./05-storage-topology-win7.md).
- **Network:** Intel **e1000**
- **Graphics:** **VGA**
- **Input:** PS/2 keyboard + mouse (or Aero’s default input devices)
- **Audio:** HDA / Intel HD Audio (optional)

Then:

1. Attach your Windows 7 ISO as the virtual CD/DVD.
2. Boot the VM and complete Windows Setup normally.
3. Confirm you can reach a stable desktop.

Recommended (but optional): take a snapshot/checkpoint here if your host environment supports it.

### Optional (recommended for x64): install SHA-2 updates before Guest Tools

If you expect to use **SHA-256 / SHA-2-signed** driver packages, install the required SHA-2 updates (commonly **KB3033929**, and sometimes also **KB4474419**) while you are still on baseline devices (AHCI/IDE/e1000/VGA). This avoids confusing “unsigned driver” failures later.

See: [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md#issue-missing-kb3033929-sha-256-signature-support)

## Step 2: Mount `aero-guest-tools.iso`

After you have a working Windows 7 desktop:

1. Eject/unmount the Windows installer ISO.
2. Mount/insert `aero-guest-tools.iso` as the virtual CD/DVD.
3. In Windows 7, open **Computer** and verify you see the CD drive.

If you were given `aero-guest-tools.zip` instead of an ISO, you can extract it on the host and copy the extracted folder into the VM (for example into `C:\AeroGuestTools\media\`) and run `setup.cmd` from there.

### Third-party notices

Guest Tools media includes `THIRD_PARTY_NOTICES.md` at the ISO/zip root and may include
additional upstream license/notice texts under `licenses/virtio-win/` (when the media
was built from a virtio-win distribution). Review these files if you are redistributing
the media.

### Where Guest Tools writes logs/reports

Regardless of whether you run Guest Tools from the mounted CD/DVD or from a copied folder, the scripts write their output to:

- `C:\AeroGuestTools\`

### Optional: copy Guest Tools to the local disk

Running directly from the mounted CD/DVD is fine. Copying the files locally is optional, but can make it easier to re-run Guest Tools without re-mounting the ISO.

1. Create a folder such as `C:\AeroGuestTools\media\`
2. Copy all files from the Guest Tools CD into `C:\AeroGuestTools\media\`

## Step 3: Run `setup.cmd` as Administrator

1. Navigate to the Guest Tools folder:
   - Mounted CD/DVD (for example `X:\`), **or**
   - `C:\AeroGuestTools\media\` (if you copied the files locally)
2. Right-click `setup.cmd` → **Run as administrator**.
3. Accept any UAC prompts.

During installation you may see driver install prompts:

- **Windows 7 x86:** Windows may warn about unsigned drivers. Choose **Install this driver software anyway** (only if you trust the Guest Tools you’re using).
- **Windows 7 x64:** Windows enforces kernel driver signatures. Guest Tools behavior is controlled by `manifest.json` `signing_policy`:
  - `test`: installs certificate(s) from `certs\` (when present) and may prompt to enable **Test Signing** so the drivers can load.
  - `production` / `none`: production/WHQL-signed drivers are expected; Guest Tools will not prompt to enable Test Signing by default.

When `setup.cmd` finishes, reboot Windows if prompted.

### x64: `setup.cmd` signing policy (Test Signing / nointegritychecks)

Guest Tools media built by `tools/packaging/aero_packager` includes a `manifest.json` that describes signing expectations via `signing_policy`:

- `test`: intended for test-signed/custom-signed drivers.
  - The packager requires shipping certificate files under `certs\` and `setup.cmd` will install them.
  - On Windows 7 x64, `setup.cmd` may prompt to enable **Test Signing** (or enable it automatically under `/force`).
- `production`: intended for production/WHQL-signed drivers (no custom root cert, no Test Mode watermark expected).
  - `certs\` may be empty (or docs-only).
  - `setup.cmd` will not prompt to enable Test Signing by default.
- `none`: same as `production` for certificate/Test Signing behavior (development use).

If you are building your own Guest Tools media for WHQL/production-signed drivers, package with:

- `aero_packager --signing-policy production`

On Windows 7 x64, **test-signed** Guest Tools builds (`signing_policy=test`) may ask:

- `Enable Test Signing now (recommended for test-signed drivers)? [Y/N]`

If you are using test-signed/custom-signed drivers, choose **Y**. A reboot is required before the setting takes effect.

#### Override flags

Explicit command-line flags override the manifest:

- `setup.cmd /testsigning` or `setup.cmd /forcetestsigning` (enable Test Signing without prompting)
- `setup.cmd /nointegritychecks` or `setup.cmd /forcenointegritychecks` (enable `nointegritychecks` without prompting; **not recommended**)
- `setup.cmd /forcesigningpolicy:none|test|production` (override `manifest.json` `signing_policy`; legacy aliases: `testsigning`→`test`, `nointegritychecks`→`none`)

To keep the current boot policy unchanged, use:

- `setup.cmd /notestsigning` (skip changing the Test Signing state)

`setup.cmd /force` only implies `/testsigning` on x64 when `signing_policy=test`.

If `setup.cmd` fails or prints warnings, **do not** switch the boot disk to virtio-blk yet. Review:

- `C:\AeroGuestTools\install.log`

It is safe (and often recommended) to re-run `setup.cmd` after fixing the underlying problem.

### `setup.cmd` output files

If you need to troubleshoot an installation, start by reviewing:

- `C:\AeroGuestTools\install.log`

Depending on the Guest Tools version, you may also see:

- `C:\AeroGuestTools\installed-driver-packages.txt`
- `C:\AeroGuestTools\installed-certs.txt`

### Optional `setup.cmd` flags (advanced)

Guest Tools may support additional command-line flags. Common examples include:

- `setup.cmd /stageonly` (only stages drivers into the driver store)
- `setup.cmd /testsigning` / `setup.cmd /forcetestsigning` (x64: enable Test Signing without prompting)
- `setup.cmd /notestsigning` (x64: keep Test Signing state unchanged)
- `setup.cmd /nointegritychecks` / `setup.cmd /forcenointegritychecks` (x64: enable `nointegritychecks`; **not recommended**)
- `setup.cmd /forcesigningpolicy:none|test|production` (override `manifest.json` `signing_policy`; legacy aliases: `testsigning`→`test`, `nointegritychecks`→`none`)
- `setup.cmd /noreboot` (do not prompt for reboot/shutdown at the end)
- `setup.cmd /skipstorage` (alias: `/skip-storage`)  
  Skip boot-critical virtio-blk storage pre-seeding. Intended for partial Guest Tools payloads (for example AeroGPU-only development builds).  
  **Unsafe to switch the boot disk from AHCI → virtio-blk** until you later run `setup.cmd` again **without** `/skipstorage` (or manually replicate the registry/service steps).

To see the supported options for your build, you can also run:

- `setup.cmd /?`

If the Guest Tools media includes a `README.md`, consult it for the definitive list of supported flags for your build.

### x64: “Test Mode” is expected if test signing is enabled

If Guest Tools enables test signing on Windows 7 x64, you may see a desktop watermark like:

- `Test Mode Windows 7 ...`

This is normal for test-signed drivers. Only disable test signing after you have confirmed you are using production-signed drivers (see the troubleshooting guide).

## What `setup.cmd` changes

The exact actions depend on the Guest Tools version, but the workflow generally includes:

### 1) Certificate store

Installs any certificate file(s) shipped under `certs\` (`*.cer`, `*.crt`, `*.p7b`) into the **Local Machine** certificate stores so Windows can trust Aero’s driver packages.

This step is policy-driven:

- If `signing_policy=test` (or certificate files are present), `setup.cmd` installs them.
- If no certificate files are present and `signing_policy` does not require them, `setup.cmd` logs and continues.

- **Trusted Root Certification Authorities**
- **Trusted Publishers**

### 2) Boot configuration (BCD)

Updates the boot configuration database via `bcdedit` when needed (based on `manifest.json` `signing_policy` or explicit override flags), for example:

- Enabling **Test Signing** (`testsigning on`) so test-signed kernel drivers load on Windows 7 x64 (typically only when `signing_policy=test`, unless you explicitly pass `/testsigning`).
- Optionally enabling `nointegritychecks` (disables signature enforcement entirely; **not recommended**).

Reboot is required after changing BCD settings.

### 3) Driver store / PnP staging

Stages the Aero drivers into the Windows driver store (so that when you later switch devices, Windows can bind the correct drivers automatically). Guest Tools typically:

- adds every `.inf` under `drivers\x86\` (on Win7 x86) or `drivers\amd64\` (on Win7 x64) using `pnputil`,
- and may attempt an immediate install for any matching devices present (unless `/stageonly` is used).

### 4) Registry / service configuration

Configures driver services and boot-critical settings (especially important for storage drivers), for example:

- Ensuring the **virtio storage** driver is set to start at boot when needed.
- Setting device/service parameters under `HKLM\SYSTEM\CurrentControlSet\Services\...`
- Pre-seeding `HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase\...` entries for virtio-blk PCI IDs so the system can boot after switching **AHCI → virtio-blk**.

## If `setup.cmd` fails: manual install (advanced)

If `setup.cmd` fails (or you prefer to install components manually), you can typically do the same work yourself.

> The exact file names and folder layout inside `aero-guest-tools.iso` may vary by version. The commands below use common paths used by Aero Guest Tools (for example `X:\drivers\` and (when present) `X:\certs\`), but always prefer the layout on your media.

### 1) Import the driver signing certificate (Local Machine, if present/required)

If the Guest Tools media includes certificate file(s) (commonly under `X:\certs\` as `.cer`, `.crt`, or `.p7b`), import them.

If your Guest Tools media has `manifest.json` `signing_policy=none`, it may ship **no** certificate files and this step is typically unnecessary (WHQL/production-signed drivers).

From an elevated Command Prompt:

- `certutil -addstore -f Root X:\certs\your-cert.cer`
- `certutil -addstore -f TrustedPublisher X:\certs\your-cert.cer`

(`X:` is usually the Guest Tools CD drive letter.)

### 2) Enable test signing (Windows 7 x64, if required)

From an elevated Command Prompt:

- `bcdedit /set {current} testsigning on`
- Reboot

If you are using production-signed drivers, keep test signing off.

### 3) Stage/install drivers into the driver store

Use either `pnputil` (Windows 7 built-in) or DISM:

- Recommended: DISM (recursively add everything under the correct arch folder):
  - Windows 7 x64:
    - `dism /online /add-driver /driver:X:\drivers\amd64\ /recurse`
  - Windows 7 x86:
    - `dism /online /add-driver /driver:X:\drivers\x86\ /recurse`
- Alternative (if you prefer `pnputil`):
  - `pnputil -i -a X:\drivers\amd64\some-driver.inf` (x64) or `X:\drivers\x86\some-driver.inf` (x86)
  - Tip: the AeroGPU display driver INF is typically `aerogpu_dx11.inf` (DX11-capable). Some older/custom builds may also ship `aerogpu.inf` (DX9-only).
    In typical Guest Tools layouts it is:
    - `X:\drivers\amd64\aerogpu\aerogpu_dx11.inf` (x64)
    - `X:\drivers\x86\aerogpu\aerogpu_dx11.inf` (x86)
  - To bulk-install multiple INFs from an elevated Command Prompt:
    - Windows 7 x64:
      - `for /r "X:\drivers\amd64" %i in (*.inf) do pnputil -i -a "%i"`
    - Windows 7 x86:
      - `for /r "X:\drivers\x86" %i in (*.inf) do pnputil -i -a "%i"`
    - If you put this into a `.cmd` file, use `%%i` instead of `%i`:
      - Windows 7 x64:
        - `for /r "X:\drivers\amd64" %%i in (*.inf) do pnputil -i -a "%%i"`
      - Windows 7 x86:
        - `for /r "X:\drivers\x86" %%i in (*.inf) do pnputil -i -a "%%i"`

After staging, reboot once while still on baseline devices.

### 4) Pre-seed boot-critical virtio-blk storage (required before switching AHCI → virtio-blk)

If you plan to boot Windows from **virtio-blk**, you must also set up boot-critical storage plumbing (otherwise switching the boot disk commonly results in `0x0000007B INACCESSIBLE_BOOT_DEVICE`).

The safest approach is to get `setup.cmd` working, because it:

- configures the storage driver service as BOOT_START, and
- pre-seeds `HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase\...` for the expected virtio-blk PCI IDs (based on `X:\config\devices.cmd`).

If you cannot run `setup.cmd`, do **not** switch the boot disk to virtio-blk until you have replicated those registry/service steps.

If you are using a **partial** Guest Tools build that does not include the virtio-blk storage driver (for example a GPU-only development build), you can still run:

- `setup.cmd /skipstorage`

to install certificates and stage the non-storage drivers. In that case, **leave the boot disk on AHCI** until you later re-run `setup.cmd` without `/skipstorage` using media that includes the virtio-blk driver (or you manually configure the service + CriticalDeviceDatabase keys).

## Step 4: Reboot (still on baseline devices)

After running Guest Tools, reboot once while still using baseline devices. This confirms the OS still boots normally before changing storage/network/display hardware.

Tip: `setup.cmd` may offer an interactive choice at the end (Reboot/Shutdown/No action). Choosing **Reboot** matches this step. If you choose **Shutdown**, consider booting once on baseline devices before you switch the boot disk to virtio-blk.

If `setup.cmd` enabled **Test Signing** or `nointegritychecks`, the reboot is required before those BCD settings take effect.

## Step 5: Switch to virtio + Aero GPU (recommended order)

To reduce the chance of an unrecoverable boot issue, switch devices **in stages** and verify Windows boots between each step.

### Stage A: switch storage (AHCI → virtio-blk)

If you installed Guest Tools with `setup.cmd /skipstorage`, do **not** perform this stage yet. Leave the boot disk on **AHCI** until you re-run `setup.cmd` without `/skipstorage` using media that includes the virtio-blk driver.

1. Shut down Windows cleanly.
2. In Aero’s VM settings, switch the **system disk controller** from **AHCI** to **virtio-blk**.
3. Boot Windows.

Expected behavior:

- Windows boots to desktop.
- It may install new devices and ask for another reboot.

If you see firmware-level errors like “No bootable device” or “BOOTMGR is missing”, see:

- [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md#issue-no-bootable-device-or-bootmgr-is-missing-after-switching-storage)

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

If you get a black screen after switching to the Aero GPU, switch back to **VGA** and follow the recovery steps in:

- [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md#issue-black-screen-after-switching-to-the-aero-gpu)

### Stage D: switch input (PS/2 → virtio-input) (optional)

If your VM settings expose input devices separately (and you want the virtio input stack):

1. Shut down Windows.
2. Switch input from **PS/2** to **virtio-input**.
3. Boot Windows.

Notes:

- Aero’s in-tree Win7 virtio-input driver package (`aero_virtio_input.inf`) is **revision-gated** to the `AERO-W7-VIRTIO` v1 contract (`REV_01`).
  If your VMM exposes a `REV_00` virtio-input device (common in QEMU defaults), the driver will not bind; configure the device to report `REV_01`
  (for example `x-pci-revision=0x01`, ideally with `disable-legacy=on`).

If you lose keyboard/mouse input, power off and switch back to **PS/2**. Then troubleshoot:

- [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md#issue-lost-keyboardmouse-after-switching-to-virtio-input)

### Stage E: switch audio (HDA → virtio-snd) (optional)

Virtio audio does not affect boot, so treat it as an optional final step:

1. Shut down Windows.
2. Switch audio from **HDA** to **virtio-snd**.
3. Boot Windows and test audio:
   - Control Panel → **Sound** → **Playback** tab: confirm the virtio-snd output endpoint exists.
   - Control Panel → **Sound** → **Recording** tab: confirm the virtio-snd capture endpoint exists.

## Step 6: Run `verify.cmd` and interpret `report.txt`

After you can boot with virtio + Aero GPU:

1. Run `verify.cmd` from the Guest Tools media:
   - Mounted CD/DVD (for example `X:\verify.cmd`), or
   - `C:\AeroGuestTools\media\verify.cmd` (if you copied the files locally)
2. Right-click `verify.cmd` → **Run as administrator**
3. Open:
   - `C:\AeroGuestTools\report.txt`

Running `verify.cmd` typically writes:

- `C:\AeroGuestTools\report.txt` (human-readable)
- `C:\AeroGuestTools\report.json` (machine-readable)

At the end, `verify.cmd` prints an overall status:

- `Overall: PASS` (exit code 0)
- `Overall: WARN` (exit code 1)
- `Overall: FAIL` (exit code 2+)

### Optional `verify.cmd` parameters (advanced)

Some Guest Tools builds support extra diagnostics flags, for example:

- `verify.cmd -PingTarget 192.168.0.1` (override the ping target)
- `verify.cmd -PlayTestSound` (attempt to play a test sound)

If `-PingTarget` is not provided, the script may attempt to ping the default gateway (if present).

Depending on your Guest Tools version, the report may include:

- Guest Tools build metadata (if `manifest.json` is present) so you can confirm which ISO/zip build you are using
- Guest Tools config (`config\devices.cmd`) contents (service name + expected PCI IDs), which affects boot-critical storage checks
- OS version and architecture (x86 vs x64)
- Whether **KB3033929** is installed (required for validating many SHA-256-signed driver catalogs on Windows 7)
- Whether signature enforcement is configured correctly (for example `testsigning` and/or `nointegritychecks`)
  - `nointegritychecks` disables signature validation entirely and is generally not recommended; prefer properly signed/test-signed drivers + the correct certificate/updates.
- Whether the Aero driver certificate(s) are installed into the expected certificate stores (**Local Machine** `Root` + `TrustedPublisher`)
- Device/driver binding status (Device Manager health) for:
  - virtio-blk storage
  - virtio-net networking
  - virtio-snd audio
  - virtio-input
  - Aero GPU / virtio-gpu graphics
- AeroGPU D3D9 UMD DLL placement (on Win7 x64 this includes the WOW64 UMD under `C:\Windows\SysWOW64\`, required for 32-bit D3D9 apps)

#### Note: how `verify.cmd` detects virtio-snd audio binding

The **Device Binding: Audio (virtio-snd)** check in `report.txt` is derived from `verify.ps1` scanning `Win32_PnPEntity` and attempting to match the virtio-snd PCI device by:

- the Windows **driver service name** bound to the device (preferred), and
- the expected virtio-snd PCI Hardware IDs from `config\devices.cmd` (fallback).

`verify.ps1` reads `AERO_VIRTIO_SND_SERVICE` from `config\devices.cmd` and checks it first, then falls back to common service names:

- `aero_virtio_snd` (Aero clean-room, canonical)
- `aeroviosnd` (legacy Aero clean-room)
- `aeroviosnd_legacy` (Aero QEMU compatibility package; transitional virtio-snd `PCI\VEN_1AF4&DEV_1018`)
- `aeroviosnd_ioport` (Aero legacy I/O-port bring-up package; transitional virtio-snd `PCI\VEN_1AF4&DEV_1018&REV_00`)
- `viosnd` (upstream virtio-win)
- `aerosnd`
- `virtiosnd`

If you are using a virtio-snd driver with a different service name, copy the Guest Tools media to a writable folder and edit `config\devices.cmd` to override:

```cmd
set "AERO_VIRTIO_SND_SERVICE=your-service-name"
```

Repo note: in this repository, `guest-tools/config/devices.cmd` is generated from `docs/windows-device-contract.json` via `scripts/generate-guest-tools-devices-cmd.py`. Update the JSON manifest + regenerate rather than editing the repo copy directly.

CI-style drift check (no rewrite):

```bash
python3 scripts/ci/gen-guest-tools-devices-cmd.py --check
```

Missing virtio-snd devices are reported as **WARN** (audio is optional).
- Boot-critical storage readiness for switching AHCI → virtio-blk:
  - storage service `Start=0` (BOOT_START)
  - `CriticalDeviceDatabase` mappings for the expected virtio-blk PCI HWIDs (prevents `0x7B INACCESSIBLE_BOOT_DEVICE`)

If `report.txt` shows failures or warnings, see:

- [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md)

## Safe rollback path (if virtio-blk boot fails)

If Windows fails to boot after switching to **virtio-blk** (common symptoms: boot loop or a BSOD like `0x0000007B INACCESSIBLE_BOOT_DEVICE`):

1. Power off the VM.
2. Switch the disk controller back to **AHCI** in Aero’s VM settings.
3. Boot Windows.
4. Re-run `setup.cmd` as Administrator and reboot once more.
5. Try switching to virtio-blk again (and avoid changing multiple device classes at once).

### Rollback if virtio-net fails

If you lose networking after switching **e1000 → virtio-net**:

1. Power off the VM.
2. Switch the NIC back to **e1000**.
3. Boot Windows and troubleshoot virtio-net driver binding from a working desktop.

### Rollback if virtio-input fails

If you lose keyboard/mouse input after switching **PS/2 → virtio-input**:

1. Power off the VM.
2. Switch input back to **PS/2**.
3. Boot Windows and troubleshoot virtio-input driver binding from a working desktop.

### Rollback if Aero GPU fails

If you get a black/blank screen after switching **VGA → Aero GPU**:

1. Power off the VM.
2. Switch graphics back to **VGA**.
3. Boot Windows and follow the recovery steps in:
   - [`docs/windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md#issue-black-screen-after-switching-to-the-aero-gpu)

### Rollback if virtio-snd fails

If audio stops working after switching **HDA → virtio-snd**, you can always switch back to **HDA**. Audio problems do not affect boot.

## Optional: uninstall Guest Tools

Guest Tools also includes `uninstall.cmd` for best-effort cleanup (useful for testing or reverting a VM back to baseline drivers).

1. Boot Windows.
2. (Recommended) If your boot disk is currently virtio-blk, switch back to **AHCI** first and boot successfully.
3. Run `uninstall.cmd` as Administrator from:
   - the mounted CD/DVD, or
   - `C:\AeroGuestTools\media\` (if you copied the files locally)
4. Review:
   - `C:\AeroGuestTools\uninstall.log`

Uninstall is best-effort and may not remove drivers that are currently in use.

Depending on the Guest Tools version, `uninstall.cmd` may also offer to disable Test Signing and/or `nointegritychecks` if they were enabled by Aero Guest Tools.

## Optional: Slipstream SHA-2 updates and drivers into your Windows 7 ISO

Slipstreaming is optional, but can reduce first-boot driver/signature problems (especially for offline installs).

**Rules:**

- Only modify ISOs you legally own.
- Do not redistribute the resulting ISO.

### What you can slipstream

- **KB3033929** (SHA-256 signature support)
- **KB4474419** (additional SHA-2 code signing support; may require servicing stack updates depending on your base image)
- **KB4490628** (servicing stack update; a common prerequisite for installing newer updates like KB4474419)
- Aero driver `.inf` packages (virtio-blk/net and optionally Aero GPU)

### High-level DISM approach (Windows host)

On a Windows 10/11 host (or a Windows VM), you can use DISM:

1. Copy ISO contents to a working folder (example: `C:\win7-iso\`).
2. Identify your `install.wim` index:
   - `dism /Get-WimInfo /WimFile:C:\win7-iso\sources\install.wim`
3. Mount `install.wim` (example index `1`):
   - `mkdir C:\wim\mount`
   - `dism /Mount-Wim /WimFile:C:\win7-iso\sources\install.wim /Index:1 /MountDir:C:\wim\mount`
4. Add the update packages you need (repeat `/Add-Package` per update).
   - Tip: when servicing stack updates are involved, add the SSU first (for example **KB4490628**), then add other updates.
   - Example:
     - `dism /Image:C:\wim\mount /Add-Package /PackagePath:C:\updates\KB4490628-x64.msu`
     - `dism /Image:C:\wim\mount /Add-Package /PackagePath:C:\updates\KB3033929-x64.msu`
     - `dism /Image:C:\wim\mount /Add-Package /PackagePath:C:\updates\KB4474419-x64.msu`
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
