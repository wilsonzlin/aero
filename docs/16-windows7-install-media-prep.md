# 16 - Windows 7 SP1 Install Media Preparation (Slipstreaming)

## Overview

Aero cannot distribute Microsoft Windows binaries. Users (or developers) must supply their own **Windows 7 SP1** installation ISO and prepare it locally.

This document describes an **auditable, reproducible** process for preparing a Win7 SP1 install image that:

- Loads **Aero storage drivers** during Windows Setup (WinPE), so disks are visible.
- Installs Aero drivers into the **installed OS image** so Windows can boot after installation.
- Configures **driver signature policy** (prefer test-signing; fallback to disabling integrity checks).
- Installs the **Aero test root certificate** into WinPE and the installed OS so test-signed drivers are trusted.

This is written to be executable manually today, and to serve as a reference for future automation (see `tools/win7-slipstream/templates/`).

Related references in this repo:

- `docs/16-win7-image-servicing.md` (more detailed, Windows-first DISM/reg/bcd workflows)
- `docs/16-win7-unattended-install.md` (unattended install details and hooks)
- `tools/win7-slipstream/patches/README.md` (auditable `.reg` patches for offline BCD + SOFTWARE hives)

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

Templates exist at `tools/win7-slipstream/templates/`.

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

Keep Aero additions under a single directory in the ISO root (example):

```
aero/
  certs/
    aero-test-root.cer
  drivers/
    winpe/        # storage drivers needed during Setup
    system/       # drivers to be present in installed OS
  scripts/
    SetupComplete.cmd
    FirstLogon.ps1
```

Even when using DISM to inject drivers, keeping a copy of the exact inputs on the ISO is useful for auditing and later debugging.

---

## Strategy A: Windows-only offline injection (DISM + bcdedit)

### 0) Prerequisites

- Fast path: if you just want a repeatable one-command workflow (drivers + offline cert trust + BCD patching), use:
  - `tools/windows/patch-win7-media.ps1` (see `tools/windows/README.md`)

- Windows 10/11 host (or Windows VM) with:
  - DISM (`dism.exe`) available (built-in).
  - Optional: Windows ADK `oscdimg.exe` for ISO rebuild.
- Your Aero driver `.inf` directories for the correct architecture.
- Aero test root certificate file: `aero-test-root.cer` (DER or Base64 is fine).

### 1) Copy ISO contents to a working directory

Mount the ISO and copy its contents:

```powershell
$IsoDrive = "E:"                       # mounted ISO
$WorkDir  = "C:\\win7-slipstream\\iso"
New-Item -ItemType Directory -Force $WorkDir | Out-Null
robocopy "$IsoDrive\\" "$WorkDir\\" /E
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

Recommended tooling (preferred over manual registry editing):

```powershell
# After mounting a WIM index to $Mount:
win-offline-cert-injector `
  --windows-dir "$Mount" `
  --store ROOT `
  --store TrustedPublisher `
  --cert "C:\\path\\to\\aero-test-root.cer"
```

PowerShell helper (repeat for each mounted image where you need trust):

```powershell
function Add-CertToOfflineHive {
  param(
    [Parameter(Mandatory=$true)][string]$OfflineSoftwareHivePath,
    [Parameter(Mandatory=$true)][string]$CertPath
  )

  # Use X509Certificate2 so PEM/Base64-encoded .cer files work too.
  $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($CertPath)
  $thumb = $cert.Thumbprint.ToUpperInvariant()
  $der = $cert.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Cert)

  reg load HKLM\\AERO_OFFLINE "$OfflineSoftwareHivePath" | Out-Null

  foreach ($store in @("ROOT","TrustedPublisher")) {
    $k = "HKLM\\AERO_OFFLINE\\Microsoft\\SystemCertificates\\$store\\Certificates\\$thumb"
    reg add $k /f | Out-Null
    reg add $k /v Blob /t REG_BINARY /d ([BitConverter]::ToString($der).Replace("-","")) /f | Out-Null
  }

  reg unload HKLM\\AERO_OFFLINE | Out-Null
}
```

Apply it to `boot.wim` indexes (mount, add cert, commit) and to the relevant `install.wim` index(es):

```powershell
$Cert = "C:\\aero\\certs\\aero-test-root.cer"

# Example: boot.wim index 2
dism /Mount-Wim /WimFile:"$WorkDir\\sources\\boot.wim" /Index:2 /MountDir:"$Mount"
Add-CertToOfflineHive -OfflineSoftwareHivePath "$Mount\\Windows\\System32\\config\\SOFTWARE" -CertPath $Cert
dism /Unmount-Wim /MountDir:"$Mount" /Commit

# Repeat for boot.wim index 1 and install.wim indexes
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
cp /path/to/aero-test-root.cer iso-root/aero/certs/
cp -R /path/to/winpe-driver-inf-dirs/* iso-root/aero/drivers/winpe/
cp -R /path/to/system-driver-inf-dirs/* iso-root/aero/drivers/system/
```

### 3) Add an `autounattend.xml`

Copy one of the templates to the ISO root and substitute placeholders:

- `tools/win7-slipstream/templates/autounattend.drivers-only.xml`
- `tools/win7-slipstream/templates/autounattend.full.xml`

Windows Setup scans the root of removable media / install media for `autounattend.xml`.

### 4) Add setup scripts (optional but recommended)

Copy scripts from `tools/win7-slipstream/templates/` into the ISO tree per the template README.

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
- `python3`

```sh
CERT_PATH="iso-root/aero/certs/aero-test-root.cer"

# Mount boot.wim index 2 (Windows Setup) read-write.
mkdir -p mnt
wimlib-imagex mount iso-root/sources/boot.wim 2 mnt --read-write

# Generate an importable .reg patch for an offline SOFTWARE hive.
# (Handles both DER and PEM certificates.)
python3 tools/win7-slipstream/scripts/cert-to-reg.py \
  --mount-key SOFTWARE \
  --out aero-cert.reg \
  "$CERT_PATH"

hivexregedit --merge --prefix 'HKEY_LOCAL_MACHINE\SOFTWARE' mnt/Windows/System32/config/SOFTWARE aero-cert.reg
wimlib-imagex unmount mnt --commit
```

Repeat this for:

- `boot.wim` index 1 and 2
- each `install.wim` index you will install (or all indexes if unsure)

You can also patch BCD stores cross-platform using the auditable `.reg` patches in `tools/win7-slipstream/patches/`:

```sh
hivexregedit --merge --prefix 'HKEY_LOCAL_MACHINE\BCD' iso-root/boot/BCD tools/win7-slipstream/patches/bcd-testsigning.reg

# If your ISO has a UEFI BCD store too:
hivexregedit --merge --prefix 'HKEY_LOCAL_MACHINE\BCD' iso-root/efi/microsoft/boot/BCD tools/win7-slipstream/patches/bcd-testsigning.reg
```

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
  -drive file=win7.qcow2,if=virtio,format=qcow2 \
  -boot d
```
