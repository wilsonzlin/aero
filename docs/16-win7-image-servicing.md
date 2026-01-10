# 16 - Windows 7 Install Media Servicing (WinPE/Setup) for Test-Signed Virtio Drivers

## Overview (why this exists)

Windows 7 **x64** enforces **kernel-mode driver signature enforcement (DSE)** very early in boot (boot-start drivers are verified/loaded by `winload.exe`/CI before most of the OS has started). If we want to boot/install Windows 7 using **test-signed** virtio storage/network drivers (e.g. virtio-blk/virtio-net) we must:

1. Ensure the boot environment is configured to allow test signatures (**`testsigning` in the BCD store**), and
2. Ensure the relevant OS environments trust the test certificate (**offline certificate injection into each image’s SOFTWARE hive**).

If either is missing, Windows Setup/WinPE may not load boot-critical drivers and installation can fail (e.g. disk not visible), or the installed OS can fail on first boot (e.g. `INACCESSIBLE_BOOT_DEVICE` when the storage driver is blocked).

This document answers, precisely:

- Which `boot.wim` and `install.wim` indices must be patched
- Where the offline registry hives and the BCD template live inside mounted images
- Repeatable `DISM` + `reg` + `bcdedit` commands to patch media

> Note: Windows 7 **x86** does not enforce kernel-mode driver signing in the same way as x64. This doc is primarily for **Windows 7 x64**.

## Automated servicing (recommended)

For a repeatable, automated implementation of the steps in this document, use:

- [`tools/windows/patch-win7-media.ps1`](../tools/windows/patch-win7-media.ps1)

It patches:

- Media BCD stores (`boot\BCD` and `efi\microsoft\boot\bcd` when present)
- Selected `boot.wim` + `install.wim` indices (driver injection optional)
- Offline certificate trust injection into each image’s SOFTWARE hive
- Offline `install.wim` `BCD-Template` so the installed OS inherits `testsigning`
- (Optional) Nested `winre.wim` inside each `install.wim` index (via `-PatchNestedWinRE`)

See [`tools/windows/README.md`](../tools/windows/README.md) for prerequisites and usage examples.

---

## Mental model: what needs patching

There are three distinct “places” that matter for test-signed boot-start drivers:

1. **The bootable BCD store(s) on the installation media** (controls how WinPE/Setup is booted)
   - BIOS: `boot\BCD`
   - UEFI: `efi\microsoft\boot\bcd` (if present)
2. **`boot.wim`** (WinPE/Setup itself; needs the test certificate, and often the drivers)
3. **`install.wim`** (the installed OS image; needs the test certificate and a patched `BCD-Template`)

---

## `boot.wim` structure (WinPE/Setup)

### Typical indices

On Windows 7 install media, `sources\boot.wim` usually contains:

- **Index 1**: Windows PE / WinRE (recovery tools; “Repair your computer”)
- **Index 2**: Windows Setup (the environment that runs `setup.exe`)

### Which index must be patched (and why)

Install media typically boots whatever `boot.wim`’s **BootIndex** is set to (commonly **2**). That means:

- **Mandatory:** Patch **`boot.wim` Index 2** (Windows Setup), because it is what the ISO normally boots into.
- **Recommended:** Patch **`boot.wim` Index 1** as well if you want **WinRE/Recovery** to also be able to see disks/network using the same custom drivers.

To avoid guessing, read the BootIndex:

```bat
dism /Get-WimInfo /WimFile:C:\iso\sources\boot.wim
```

Look for `Boot Index : 2` (or similar) in the output.

---

## `install.wim` structure (installed OS)

`sources\install.wim` contains one image per edition (Starter/Home/Pro/Ultimate, etc). Example implications:

- If you always deploy a **single known edition**, patch only that **one index**.
- If you want to keep the ISO **multi-edition**, patch **all indices** so every edition boots with the same policy/certificate.

Determine the index list:

```bat
dism /Get-WimInfo /WimFile:C:\iso\sources\install.wim
```

### Optional extra: nested WinRE (`winre.wim`)

Inside each `install.wim` image, Windows Recovery Environment is typically stored as:

`Windows\System32\Recovery\winre.wim`

If you need recovery to load the same test-signed storage/network drivers, you may also need to mount and patch that `winre.wim` (it is a WIM inside a WIM).

---

## Exact offline paths inside mounted images (what/where)

When a WIM is mounted to (say) `C:\mount\img`, these are the key files:

| Purpose | Path inside mounted image |
| --- | --- |
| SYSTEM hive (driver/service config, etc.) | `C:\mount\img\Windows\System32\Config\SYSTEM` |
| SOFTWARE hive (certificate stores live here) | `C:\mount\img\Windows\System32\Config\SOFTWARE` |
| BCD template used by Setup/`bcdboot` | `C:\mount\img\Windows\System32\Config\BCD-Template` |

---

## Certificate injection (offline) — how it works

Windows “Local Machine” certificate stores are registry-backed under the SOFTWARE hive. Conceptually:

- Store location: `HKLM\SOFTWARE\Microsoft\SystemCertificates\<STORE>\Certificates`
- Each certificate is a subkey named by its **SHA-1 thumbprint** (no spaces, typically uppercase)
- The certificate entry is stored as one or more values (typically a `REG_BINARY` value named **`Blob`**)
  written by CryptoAPI's registry-backed cert store provider (**not guaranteed to be raw DER**)

For test-signed kernel drivers, it is common to install the test certificate into both:

- `ROOT` (Trusted Root Certification Authorities)
- `TrustedPublisher` (Trusted Publishers)

Offline, that becomes:

- `HKLM\<OFFLINE_SOFTWARE>\Microsoft\SystemCertificates\ROOT\Certificates\<thumbprint>\Blob`
- `HKLM\<OFFLINE_SOFTWARE>\Microsoft\SystemCertificates\TrustedPublisher\Certificates\<thumbprint>\Blob`

### Recommended: inject a certificate into an offline-mounted image using CryptoAPI

Rather than hand-writing registry values, use the Windows-native `tools/win-offline-cert-injector`,
which loads the offline SOFTWARE hive and uses CryptoAPI to create the exact registry-backed store entry.

Run from an elevated PowerShell prompt:

```powershell
$MountDir = "C:\mount\boot2"  # change per image
$CertPath = "C:\certs\AeroTestRoot.cer"

# Build once (or use a prebuilt binary)
cd tools\win-offline-cert-injector
cargo build --release

.\target\release\win-offline-cert-injector.exe `
  --windows-dir $MountDir `
  --store ROOT --store TrustedPublisher `
  --cert $CertPath
```

---

## BCD edits required (WinPE + installed OS)

### 1) Patch the **media boot BCD store(s)** (required for WinPE/Setup)

Changing `startnet.cmd` / `winpeshl.ini` is **not sufficient** for driver signature enforcement: boot-start drivers are validated before those scripts run. The setting must be present in the **BCD store used to boot WinPE**.

On the extracted ISO contents (example `C:\iso\...`):

```bat
:: BIOS boot path (always on Win7 media)
bcdedit /store C:\iso\boot\BCD /set {default} testsigning on

:: Optional: disables integrity checks entirely (lab use only)
:: bcdedit /store C:\iso\boot\BCD /set {default} nointegritychecks on

:: UEFI boot path (only if your ISO contains it)
if exist C:\iso\efi\microsoft\boot\bcd (
  bcdedit /store C:\iso\efi\microsoft\boot\bcd /set {default} testsigning on
)
```

Notes:

- Prefer `testsigning on` when using test-signed drivers + injected cert.
- Use `nointegritychecks on` only when you understand the security implications and are in a controlled test environment.

### 2) Patch the **`BCD-Template` inside `install.wim`** (required for the installed OS)

Windows Setup creates the installed OS boot store using the template at:

`Windows\System32\Config\BCD-Template`

Patch that template **inside each `install.wim` index you intend to deploy** so newly-created boot stores inherit `testsigning`:

```bat
:: After mounting install.wim index N to C:\mount\installN
bcdedit /store C:\mount\installN\Windows\System32\Config\BCD-Template /set {default} testsigning on

:: Optional (lab only)
:: bcdedit /store C:\mount\installN\Windows\System32\Config\BCD-Template /set {default} nointegritychecks on
```

If `bcdedit` complains that `{default}` is not found, enumerate and pick the Windows Boot Loader identifier:

```bat
bcdedit /store C:\mount\installN\Windows\System32\Config\BCD-Template /enum all
```

---

## Repeatable servicing workflow (DISM/reg/bcdedit)

This is an end-to-end “do it every time” sequence. Assumes you have already extracted the ISO to `C:\iso`.

### 0) Inspect WIM indices (don’t guess)

```bat
dism /Get-WimInfo /WimFile:C:\iso\sources\boot.wim
dism /Get-WimInfo /WimFile:C:\iso\sources\install.wim
```

### 1) Patch ISO boot BCD stores (WinPE/Setup)

```bat
bcdedit /store C:\iso\boot\BCD /set {default} testsigning on
if exist C:\iso\efi\microsoft\boot\bcd (
  bcdedit /store C:\iso\efi\microsoft\boot\bcd /set {default} testsigning on
)
```

### 2) Patch `boot.wim` (WinPE/Setup)

Mount, inject cert (and optionally drivers), commit.

```bat
md C:\mount\boot2
dism /Mount-Wim /WimFile:C:\iso\sources\boot.wim /Index:2 /MountDir:C:\mount\boot2

:: Inject test cert (see PowerShell snippet above; set $MountDir=C:\mount\boot2)
:: Optional: add virtio drivers to WinPE/Setup
:: dism /Image:C:\mount\boot2 /Add-Driver /Driver:C:\drivers\virtio\win7\amd64 /Recurse

dism /Unmount-Wim /MountDir:C:\mount\boot2 /Commit
rd C:\mount\boot2
```

Optional (recommended for recovery):

```bat
md C:\mount\boot1
dism /Mount-Wim /WimFile:C:\iso\sources\boot.wim /Index:1 /MountDir:C:\mount\boot1

:: Inject test cert (set $MountDir=C:\mount\boot1)
:: Optional: add recovery drivers

dism /Unmount-Wim /MountDir:C:\mount\boot1 /Commit
rd C:\mount\boot1
```

### 3) Patch `install.wim` (installed OS image)

Repeat for each edition index you want to support:

```bat
md C:\mount\installN
dism /Mount-Wim /WimFile:C:\iso\sources\install.wim /Index:<N> /MountDir:C:\mount\installN

:: Inject test cert (set $MountDir=C:\mount\installN)

:: Patch BCD template so the installed OS boots with testsigning
bcdedit /store C:\mount\installN\Windows\System32\Config\BCD-Template /set {default} testsigning on

:: Optional: add virtio drivers into the installed OS image
:: dism /Image:C:\mount\installN /Add-Driver /Driver:C:\drivers\virtio\win7\amd64 /Recurse

dism /Unmount-Wim /MountDir:C:\mount\installN /Commit
rd C:\mount\installN
```

### 4) (Optional) Patch nested `winre.wim` inside `install.wim`

If you need recovery to understand the same storage/network devices:

1. Mount `install.wim` index N.
2. Copy `Windows\System32\Recovery\winre.wim` out to a working path.
3. Mount and patch `winre.wim` (it is usually a single-index WinPE image).
4. Replace it back into the mounted `install.wim` image.
5. Commit/unmount the `install.wim`.

---

## WinPE caveats (common pitfalls)

- **Editing `startnet.cmd` / `winpeshl.ini` does not bypass DSE** for boot-start drivers. Those scripts run *after* the kernel has already decided which boot-start drivers it will load.
- For WinPE/Setup, the critical policy is the **BCD store on the media** (`boot\BCD` / `efi\...\bcd`), not a setting inside the WIM.
- For the installed OS, patching **`BCD-Template`** inside `install.wim` is how you get `testsigning` set *before the first boot* of the deployed system.

---

## Verification checklist (don’t ship a broken ISO)

### Confirm WIM indices + BootIndex

```bat
dism /Get-WimInfo /WimFile:C:\iso\sources\boot.wim
dism /Get-WimInfo /WimFile:C:\iso\sources\install.wim
```

### Confirm BCD settings (media)

```bat
bcdedit /store C:\iso\boot\BCD /enum {default}

:: If present
bcdedit /store C:\iso\efi\microsoft\boot\bcd /enum {default}
```

Look for:

- `testsigning                Yes`
- (optional) `nointegritychecks          Yes`

### Confirm certificate presence in an offline image

```bat
reg load HKLM\OFFSOFT C:\mount\boot2\Windows\System32\Config\SOFTWARE
reg query "HKLM\OFFSOFT\Microsoft\SystemCertificates\ROOT\Certificates"
reg query "HKLM\OFFSOFT\Microsoft\SystemCertificates\TrustedPublisher\Certificates"
reg unload HKLM\OFFSOFT
```

You should see a subkey whose name matches your certificate thumbprint (no spaces).
