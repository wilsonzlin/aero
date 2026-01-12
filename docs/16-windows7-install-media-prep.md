# 16 - Windows 7 SP1 Install Media Preparation (Slipstreaming)

## Overview

Aero cannot distribute Microsoft Windows binaries. Users (or developers) must supply their own **Windows 7 SP1** installation ISO and prepare it locally.

This document describes an **auditable, reproducible** process for preparing a Win7 SP1 install image that:

- Loads **Aero storage drivers** during Windows Setup (WinPE), so disks are visible.
- Installs Aero drivers into the **installed OS image** so Windows can boot after installation.
- Configures **driver signature policy** (prefer test-signing; fallback to disabling integrity checks).
- Installs the **Aero test root certificate** into WinPE and the installed OS so test-signed drivers are trusted.

Note: If you are using Aero’s **baseline** Windows 7 install topology (AHCI HDD + IDE/ATAPI CD-ROM),
Windows Setup should be able to see the disk using Windows 7’s in-box AHCI driver. This document is
primarily needed when you want to install/boot using **paravirtual** or otherwise non-inbox storage
devices (e.g. virtio-blk). For the baseline topology details (canonical PCI BDFs, attachment mapping,
and interrupt routing), see [`docs/05-storage-topology-win7.md`](./05-storage-topology-win7.md).

This is written to be executable manually today, and to serve as a reference for future automation (see `tools/win7-slipstream/templates/`).

Related references in this repo:

- `docs/16-win7-image-servicing.md` (more detailed, Windows-first DISM/reg/bcd workflows)
- `docs/16-win7-unattended-install.md` (unattended install details and hooks)
- `docs/17-win7-unattend-validation.md` (practical validation/troubleshooting playbook for unattended installs with a separate config ISO)
- `docs/win7-bcd-offline-patching.md` (BCD internals and robust offline patching strategy)
- `windows/win7-sp1/unattend/` (ready-to-edit `autounattend.xml` templates and Win7-compatible post-install scripts)
- `tools/win7-slipstream/patches/README.md` (auditable `.reg` patches for offline BCD + SOFTWARE hives)
- `tools/bcd_patch/` (cross-platform CLI to patch BCD stores without `bcdedit.exe`)
- `tools/win-offline-cert-injector/` (Windows-native CLI for injecting certs into offline SOFTWARE hives)
- `tools/windows/patch-win7-media.ps1` (Windows-only helper that applies the same kinds of patches programmatically)

---

## Supported input ISOs (expected layout)

This process assumes a Windows 7 **SP1** ISO (x86 or x64) with the standard layout:

- `sources/boot.wim` (WinPE + Windows Setup)
- `sources/install.wim` (the OS images / editions)
- BIOS boot BCD store: `boot/BCD`
- UEFI boot BCD store (x64 media typically): `efi/microsoft/boot/BCD`

Notes:

- Some OEM media may have a different layout or additional boot entries; always validate the structure first.
- x86 media may not include `efi/…` at all (BIOS-only).
- Aero driver injection must match the ISO architecture (x86 drivers for x86 media; amd64 drivers for x64 media).
- On case-sensitive host filesystems (Linux/macOS), extracted ISO file paths may differ in case (for example `efi/microsoft/boot/bcd`). Treat these paths as **case-insensitive identifiers**, and verify the actual extracted filenames before running commands that reference them.

### Quick structure checks

On any platform:

```sh
# Confirm WIM files exist
test -f sources/boot.wim && test -f sources/install.wim

# Optional: list WIM indexes (requires wimlib)
wimlib-imagex info sources/boot.wim
wimlib-imagex info sources/install.wim
```

---

## What must be patched (minimum)

To boot and install Windows 7 with Aero’s custom kernel-mode drivers (especially storage, and likely graphics later), the install media typically needs all of the following:

1. **WinPE (boot.wim) must contain the required drivers**
   - At minimum: storage driver(s) so Setup can see the disk.
   - Recommended: inject into both indexes of `boot.wim` (WinPE + Setup).

2. **Install image (install.wim) must contain the required drivers**
   - At minimum: storage driver(s) so the installed OS can boot.
   - Recommended: inject into the specific edition index you will install, or all indexes if unsure.

3. **WinPE boot BCD settings on the ISO**
   - Patch both `boot/BCD` (BIOS) and `efi/microsoft/boot/BCD` (UEFI if present).
   - Enable the required signature mode for WinPE so Setup can load Aero drivers:
     - Preferred: `testsigning on` (with Aero test root cert installed in WinPE).
     - Fallback (emulator-only): `nointegritychecks on` (security tradeoff; see below).

4. **Installed OS boot settings**
   - Prefer patching the image’s BCD template at:
     - `install.wim:<index>\Windows\System32\config\BCD-Template`
   - This ensures newly installed Windows boots with the same signature mode as required for Aero drivers.

5. **Certificate trust (offline registry)**
   - Install Aero’s test root certificate into the **offline SOFTWARE hive** for:
     - WinPE image(s): `boot.wim:<index>\Windows\System32\config\SOFTWARE`
     - Installed OS image(s): `install.wim:<index>\Windows\System32\config\SOFTWARE`
   - This enables the OS to trust **test-signed** Aero drivers from first boot.

---

## Driver injection strategies

### Strategy A (Windows host): full offline injection with DISM (recommended)

Use Windows DISM + bcdedit to inject drivers/certs directly into `boot.wim` and `install.wim`, and patch BCD stores.

Pros:
- Most direct and well-supported.
- Easy to validate using DISM.

Cons:
- Requires Windows (or Windows VM) with DISM available.

### Strategy B (cross-platform host): unattend-based injection (drivers staged on media)

Stage drivers on the install media and use `autounattend.xml` to:

- Point WinPE (`PnpCustomizationsWinPE`) at driver directories on the media.
- Point offline servicing (`PnpCustomizationsNonWinPE`) at driver directories to stage into the installed image.

Pros:
- Works on Linux/macOS without DISM for *driver injection*.
- Keeps changes visible as files on the media.

Cons:
- You still need to handle signature policy + certificate trust.
- For boot-critical drivers, you still must ensure the installed OS trusts the signing cert from first boot.

Ready-to-edit unattend examples live at `windows/win7-sp1/unattend/`. Golden-reference templates (placeholders intended for future tooling) live at `tools/win7-slipstream/templates/`.

---

## Driver signature strategy

### Preferred: test-signed drivers + Aero test root cert + testsigning enabled

This is the best balance for emulator development:

- Drivers are cryptographically signed (auditability).
- Windows is explicitly placed into test mode.
- Trust is limited to your test root cert (instead of “trust everything”).

Requirements:

- All Aero kernel-mode drivers should be signed with an Aero test certificate.
- The corresponding test root cert must be present in:
  - `LocalMachine\\Root` (Trusted Root Certification Authorities)
  - `LocalMachine\\TrustedPublisher` (Trusted Publishers)
- BCD must have `testsigning on` (WinPE and installed OS).

### Fallback: disable integrity checks (`nointegritychecks`)

This is **not recommended** except for controlled environments (e.g., inside the Aero emulator during early bring-up):

- Disables kernel driver signature enforcement.
- Makes it much easier for malicious/unintended drivers to load.

Requirements:

- BCD must have `nointegritychecks on`.
- (Certificate injection is optional, but still recommended for later tightening.)

---

## Recommended media-side file layout (for auditing)

There are two common layouts, depending on whether you are directly modifying the Windows ISO or supplying a separate “config media” ISO/USB for unattend.

### Option A: single `aero/` directory on the Windows ISO

Keep Aero additions under a single directory in the ISO root (example):

```
aero/
  certs/
    aero-test.cer
  drivers/
    winpe/        # storage drivers needed during Setup
    system/       # drivers to be present in installed OS
  scripts/
    SetupComplete.cmd
    FirstLogon.ps1
```

Even when using DISM to inject drivers, keeping a copy of the exact inputs on the ISO is useful for auditing and later debugging.

### Option B: `%configsetroot%` “configuration set” layout (recommended for unattend workflows)

If you use the repo’s architecture-specific templates in `windows/win7-sp1/unattend/`, they assume a config-media layout like:

```
<config-media-root>/
  autounattend.xml
  Drivers/
    WinPE/
      amd64/
      x86/
    Offline/
      amd64/
      x86/
  Scripts/
    SetupComplete.cmd
    InstallDriversOnce.cmd
  Cert/
    aero-test.cer
```

See `windows/win7-sp1/unattend/README.md` for the full expected structure and payload-location fallbacks.

---

## Strategy A: Windows-only offline injection (DISM + bcdedit)

### 0) Prerequisites

- Fast path: if you just want a repeatable one-command workflow (drivers + offline cert trust + BCD patching), use:
  - `tools/windows/patch-win7-media.ps1` (see `tools/windows/README.md`)

- Example:
  ```powershell
  pwsh .\tools\windows\patch-win7-media.ps1 `
    -MediaRoot C:\win7-slipstream\iso `
    -CertPath  C:\certs\aero-test.cer `
    -DriversPath C:\aero\drivers\win7 `
    -InstallWimIndices all
  ```

- Windows 10/11 host (or Windows VM) with:
  - DISM (`dism.exe`) available (built-in).
  - Optional: Windows ADK `oscdimg.exe` for ISO rebuild.
- Your Aero driver `.inf` directories for the correct architecture.
- Aero test root certificate file (DER or Base64 is fine).
  - CI output: `out/certs/aero-test.cer`

### 1) Copy ISO contents to a working directory

Mount the ISO and copy its contents:

```powershell
$IsoDrive = "E:"                       # mounted ISO
$WorkDir  = "C:\\win7-slipstream\\iso"
New-Item -ItemType Directory -Force $WorkDir | Out-Null
robocopy "$IsoDrive\\" "$WorkDir\\" /E
```

Note: Some ISO extraction/copy methods mark files as read-only. If you are doing the manual workflow below (not using `patch-win7-media.ps1`), ensure files are writable before patching:

```powershell
attrib -r "$WorkDir\\sources\\boot.wim"
attrib -r "$WorkDir\\sources\\install.wim"
attrib -r "$WorkDir\\boot\\BCD"
if (Test-Path "$WorkDir\\efi\\microsoft\\boot\\BCD") { attrib -r "$WorkDir\\efi\\microsoft\\boot\\BCD" }
```

### 2) Inject drivers into `sources/boot.wim`

`boot.wim` usually contains two indexes:

- Index 1: “Windows PE”
- Index 2: “Windows Setup”

List indexes:

```powershell
dism /Get-WimInfo /WimFile:"$WorkDir\\sources\\boot.wim"
```

Mount, add drivers, commit for each index:

```powershell
$Mount = "C:\\win7-slipstream\\mount"
New-Item -ItemType Directory -Force $Mount | Out-Null

$WinPeDrivers = "C:\\aero\\drivers\\winpe"   # directory containing .inf files

foreach ($Index in 1,2) {
  dism /Mount-Wim /WimFile:"$WorkDir\\sources\\boot.wim" /Index:$Index /MountDir:"$Mount"
  dism /Image:"$Mount" /Add-Driver /Driver:"$WinPeDrivers" /Recurse
  dism /Unmount-Wim /MountDir:"$Mount" /Commit
}
```

### 3) Inject drivers into `sources/install.wim`

List available editions (indexes):

```powershell
dism /Get-WimInfo /WimFile:"$WorkDir\\sources\\install.wim"
```

Inject drivers into the edition(s) you plan to install. If unsure, inject into all:

```powershell
$OsDrivers = "C:\\aero\\drivers\\system"

# Example: inject into index 1 only. Repeat for each desired index.
$Index = 1
dism /Mount-Wim /WimFile:"$WorkDir\\sources\\install.wim" /Index:$Index /MountDir:"$Mount"
dism /Image:"$Mount" /Add-Driver /Driver:"$OsDrivers" /Recurse
dism /Unmount-Wim /MountDir:"$Mount" /Commit
```

### 4) Install Aero test root cert into offline SOFTWARE hives (WinPE + installed OS)

Windows stores LocalMachine certificate stores under the SOFTWARE hive at:

- `…\\Microsoft\\SystemCertificates\\ROOT\\Certificates\\<thumbprint>`
- `…\\Microsoft\\SystemCertificates\\TrustedPublisher\\Certificates\\<thumbprint>`

The key name is the certificate **SHA-1 thumbprint of the certificate DER bytes** (uppercase hex, no spaces).

Note: the `Blob` value stored under `SystemCertificates` is written by CryptoAPI and is **not guaranteed to be raw DER**.
Recommended tooling (preferred over manual registry editing):

```powershell
cd tools\win-offline-cert-injector
cargo build --release --locked

$Cert = "C:\\aero\\certs\\aero-test.cer"

# After mounting a WIM index to $Mount:
.\target\release\win-offline-cert-injector.exe --windows-dir "$Mount" --store ROOT --store TrustedPublisher "$Cert"
```

### 5) Patch WinPE boot BCD on the ISO (BIOS + UEFI)

Enable test-signing (preferred):

```powershell
bcdedit /store "$WorkDir\\boot\\BCD" /set {default} testsigning on

if (Test-Path "$WorkDir\\efi\\microsoft\\boot\\BCD") {
  bcdedit /store "$WorkDir\\efi\\microsoft\\boot\\BCD" /set {default} testsigning on
}
```

Fallback (emulator-only): disable integrity checks:

```powershell
bcdedit /store "$WorkDir\\boot\\BCD" /set {default} nointegritychecks on
if (Test-Path "$WorkDir\\efi\\microsoft\\boot\\BCD") {
  bcdedit /store "$WorkDir\\efi\\microsoft\\boot\\BCD" /set {default} nointegritychecks on
}
```

If `{default}` does not exist (some OEM media), enumerate and set the correct “Windows Setup” loader entry:

```powershell
bcdedit /store "$WorkDir\\boot\\BCD" /enum all
```

### 6) Patch installed OS boot policy via `BCD-Template`

Mount an `install.wim` index and patch:

```powershell
$Index = 1
dism /Mount-Wim /WimFile:"$WorkDir\\sources\\install.wim" /Index:$Index /MountDir:"$Mount"

bcdedit /store "$Mount\\Windows\\System32\\config\\BCD-Template" /set {default} testsigning on

dism /Unmount-Wim /MountDir:"$Mount" /Commit
```

As with the ISO BCD, use `/enum all` to locate the correct loader identifier if needed.

### 7) Add `autounattend.xml` and setup scripts (optional but recommended)

See `tools/win7-slipstream/templates/README.md` for where these files must live on the ISO.

### 8) Rebuild the ISO (Windows: oscdimg)

If you have the Windows ADK installed (for `oscdimg.exe`):

```powershell
$OutIso = "C:\\win7-slipstream\\win7-aero.iso"

# Dual BIOS+UEFI boot if the source ISO supports it.
# Uses boot sectors from the extracted ISO tree (do not download these from elsewhere).
oscdimg -m -o -u2 -udfver102 `
  -bootdata:2#p0,e,b"$WorkDir\\boot\\etfsboot.com"#pEF,e,b"$WorkDir\\efi\\microsoft\\boot\\efisys.bin" `
  "$WorkDir" "$OutIso"
```

If your ISO is BIOS-only, you can omit the UEFI boot entry.

---

## Strategy B: Cross-platform driver injection (autounattend + xorriso)

This strategy avoids DISM for driver injection, but still requires you to address:

- Signature mode (WinPE BCD + installed OS boot policy).
- Certificate trust (WinPE + installed OS offline SOFTWARE hives).

On Linux/macOS you can generally do the file/WIM manipulations with open-source tools, and (if needed) run the BCD-editing parts inside a Windows VM.

Tip: You can keep Aero content on a separate “config media” ISO (drivers/certs/scripts + `autounattend.xml`) and attach it alongside the Windows ISO. Windows Setup scans attached media for `autounattend.xml`, and the repo’s templates use `%configsetroot%` for stable paths. This keeps the Windows ISO’s file tree cleaner, while still requiring offline patching of BCD/certs inside the Windows WIMs for boot-critical drivers.

If you only need an interactive “Load Driver” disk (no unattend/config scripts), CI can also optionally produce a small FAT32 driver disk (`*-fat.vhd`); see [`docs/16-driver-install-media.md`](./16-driver-install-media.md).

### 0) Prerequisites

- `xorriso` (ISO rebuild)
- `wimlib-imagex` (optional; for validation and offline hive edits)
- A tool to extract the ISO:
  - `bsdtar`, `7z`, or `xorriso -osirrox on -indev … -extract / …`

### 1) Extract ISO

Example using `7z`:

```sh
mkdir -p iso-root
7z x -oiso-root Win7SP1.iso
```

### 2) Stage Aero drivers + certs on the ISO tree

```sh
mkdir -p iso-root/aero/{certs,drivers/winpe,drivers/system,scripts}
cp /path/to/aero-test.cer iso-root/aero/certs/
cp -R /path/to/winpe-driver-inf-dirs/* iso-root/aero/drivers/winpe/
cp -R /path/to/system-driver-inf-dirs/* iso-root/aero/drivers/system/
```

### 3) Add an `autounattend.xml`

Copy an `autounattend.xml` template to the ISO root and edit as needed. Options in this repo:

- Ready-to-edit, architecture-specific templates:
  - `windows/win7-sp1/unattend/autounattend_amd64.xml`
  - `windows/win7-sp1/unattend/autounattend_x86.xml`
- Golden-reference templates (placeholders intended for future tooling):
  - `tools/win7-slipstream/templates/autounattend.drivers-only.xml`
  - `tools/win7-slipstream/templates/autounattend.full.xml`

Windows Setup scans the root of removable media / install media for `autounattend.xml`.

If your unattend references `%configsetroot%` (recommended for stable paths), ensure the `Microsoft-Windows-Setup` component sets:

```xml
<UseConfigurationSet>true</UseConfigurationSet>
```

### 4) Add setup scripts (optional but recommended)

For post-install automation (install certs, enable test mode, install drivers), the repo includes Win7-compatible scripts at:

- `windows/win7-sp1/unattend/scripts/`

You can also start from the simpler golden-reference templates in `tools/win7-slipstream/templates/` (see its README for `$OEM$` placement).

### 5) Handle signature mode + certificate trust

Even with unattend-based driver paths, you must still ensure WinPE and the installed OS can load Aero drivers:

- Patch ISO BCD (`boot/BCD` and `efi/microsoft/boot/BCD`) to enable `testsigning` (preferred) or `nointegritychecks` (fallback).
- Patch `BCD-Template` inside the target install.wim index so the installed OS boots with the same signing policy.
- Install Aero’s test root cert into offline SOFTWARE hives for boot.wim + install.wim.

If you don’t have Windows tooling available, these steps can still be done cross-platform by editing the offline registry hives directly (see `tools/win7-slipstream/patches/README.md` and the example below).

#### Example: offline certificate + BCD patching on Linux/macOS (wimlib + hivexregedit)

If you prefer not to use Windows tooling for certificate injection, you can edit the offline SOFTWARE hive directly.

This example mounts `boot.wim` index 2 read-write, generates a `.reg` certificate patch, merges it into the hive, then commits.

Prerequisites:

- `wimlib-imagex` (FUSE-based mounting; on macOS you may need macFUSE)
- `hivexregedit`

Note: `cert-to-reg.py` writes the certificate’s raw DER bytes into the `Blob` registry value. This often works, but the CryptoAPI registry-backed cert store format is **not guaranteed** to be raw DER across all environments. For the most portable patch, generate the `.reg` on Windows using `tools/win-certstore-regblob-export` (or inject directly using `tools/win-offline-cert-injector`), then apply it cross-platform with `hivexregedit`. See `tools/win7-slipstream/patches/README.md`.

```sh
CERT_PATH="iso-root/aero/certs/aero-test.cer"

# Generate `aero-cert.reg` on a Windows machine (once) using CryptoAPI, then copy it here:
#   win-certstore-regblob-export --store ROOT --store TrustedPublisher --format reg --reg-hklm-subkey SOFTWARE "$CERT_PATH" > aero-cert.reg
#
# (Fallback: tools/win7-slipstream/scripts/cert-to-reg.py can generate a best-effort .reg from raw DER,
# but it may not match CryptoAPI's exact registry-backed `Blob` representation.)

# Mount boot.wim index 2 (Windows Setup) read-write.
mkdir -p mnt
wimlib-imagex mount iso-root/sources/boot.wim 2 mnt --read-write

hivexregedit --merge --prefix 'HKEY_LOCAL_MACHINE\SOFTWARE' mnt/Windows/System32/config/SOFTWARE aero-cert.reg
wimlib-imagex unmount mnt --commit
```

Repeat this for:

- `boot.wim` index 1 and 2
- each `install.wim` index you will install (or all indexes if unsure)

You can also patch BCD stores cross-platform.

Preferred (more robust): use the repo’s cross-platform BCD patcher (`tools/bcd_patch/`), which patches multiple objects (global settings, loader settings, OS loader entries) rather than relying on inheritance quirks:

```sh
# Patch extracted ISO BCD stores (boot/BCD + efi/microsoft/boot/BCD if present).
cargo run --locked -p bcd-patch -- win7-tree --root iso-root --nointegritychecks off

# Patch BCD-Template inside a mounted install.wim index (run once per index you care about).
# Example mount root: mnt-install
cargo run --locked -p bcd-patch -- win7-tree --root mnt-install --nointegritychecks off
```

Alternative: patch via auditable `.reg` files + `hivexregedit` (see `tools/win7-slipstream/patches/README.md` for details):

```sh
hivexregedit --merge --prefix 'HKEY_LOCAL_MACHINE\BCD' iso-root/boot/BCD tools/win7-slipstream/patches/bcd-testsigning.reg

# If your ISO has a UEFI BCD store too:
hivexregedit --merge --prefix 'HKEY_LOCAL_MACHINE\BCD' iso-root/efi/microsoft/boot/BCD tools/win7-slipstream/patches/bcd-testsigning.reg
```

On Linux/macOS, ensure the `iso-root/...` paths match the actual case of the extracted files (some ISOs use `bcd` instead of `BCD`).

### 6) Rebuild the ISO (Linux/macOS: xorriso)

Example command (dual BIOS+UEFI boot):

```sh
xorriso -as mkisofs \
  -iso-level 3 -udf -J -joliet-long -D -N \
  -volid "WIN7_AERO" \
  -b boot/etfsboot.com \
  -no-emul-boot -boot-load-size 8 -boot-info-table \
  -eltorito-alt-boot \
  -e efi/microsoft/boot/efisys.bin \
  -no-emul-boot \
  -o win7-aero.iso \
  iso-root
```

Important:

- Use UDF-capable output (`-udf`, `-iso-level 3`) because `install.wim` can exceed 4GB.
- Boot images (`boot/etfsboot.com`, `efi/…/efisys.bin`) must come from the user’s original ISO tree.

---

## Reproducibility & auditing recommendations

For a reproducible and reviewable slipstream process:

- Record input ISO hash (e.g., SHA256) and the output ISO hash.
- Record:
  - WIM indexes modified (boot.wim indexes, install.wim index list).
  - Driver directories injected / staged (and their hashes).
  - Certificate thumbprint installed.
  - BCD flags set (`testsigning`, `nointegritychecks`) for both WinPE and BCD-Template.
- Keep all Aero-specific additions under `aero/` on the ISO for easy review.
- If you care about byte-for-byte ISO reproducibility, normalize timestamps in the staging tree before ISO creation and keep a stable file ordering.

---

## Validation checklist (no Windows redistribution required)

### Structure checks

- [ ] ISO tree contains `sources/boot.wim` and `sources/install.wim`
- [ ] ISO tree contains `boot/BCD`
- [ ] If UEFI boot expected: `efi/microsoft/boot/BCD` exists

### WIM checks (with wimlib or DISM)

- [ ] `boot.wim` has expected indexes (usually 1 and 2)
- [ ] `install.wim` has expected edition indexes
- [ ] Drivers present:
  - Windows: `dism /Image:<mount> /Get-Drivers`
  - Cross-platform: mount with wimlib and verify files under `Windows\\System32\\DriverStore`

### BCD checks

- [ ] `boot/BCD` WinPE loader has `testsigning` enabled (preferred) or `nointegritychecks` enabled (fallback)
- [ ] If present: `efi/microsoft/boot/BCD` matches
- [ ] `install.wim:<index>\\Windows\\System32\\config\\BCD-Template` has the same signing policy

### Certificate trust checks (offline)

For each patched image (boot.wim indexes + install.wim index):

- [ ] `Windows\\System32\\config\\SOFTWARE` contains:
  - `Microsoft\\SystemCertificates\\ROOT\\Certificates\\<thumbprint>`
  - `Microsoft\\SystemCertificates\\TrustedPublisher\\Certificates\\<thumbprint>`

### Optional smoke test (user-run)

Users can locally test their prepared ISO without sharing any Windows binaries:

- Boot the ISO in QEMU/VirtualBox.
- Attach a virtual disk that requires the Aero storage driver (for example, a virtio disk).
- Confirm Windows Setup can see the disk and proceed to partition/format.

Example (QEMU, x64):

```sh
qemu-system-x86_64 \
  -m 4096 \
  -cdrom win7-aero.iso \
  -drive file=win7.qcow2,if=none,id=drive0,format=qcow2 \
  -device virtio-blk-pci,drive=drive0,disable-legacy=on,x-pci-revision=0x01 \
  -boot d
```
