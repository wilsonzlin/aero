# Aero Guest Tools (Windows 7)

This directory is intended to be shipped as the root of the **Aero Guest Tools ISO** and run **inside** a Windows 7 SP1 VM.

It provides:

- `setup.cmd`: offline certificate + driver installer
- `uninstall.cmd`: best-effort cleanup
- `verify.cmd` / `verify.ps1`: offline diagnostics/verification
- `certs\`: public certificate(s) needed to validate Aero driver signatures
- `drivers\`: driver packages (`.inf/.cat/.sys`) for x86 + amd64
- `config\`: expected device IDs (PCI VEN/DEV pairs) used for boot-critical pre-seeding

## `setup.cmd`

Designed for the standard flow:

1. Install Windows 7 SP1 using “safe” emulated devices first (AHCI/e1000/HDA/PS2/VGA).
2. Boot Windows and run `setup.cmd` **as Administrator**.
3. Power off/reboot and switch the VM devices to Aero virtio devices (virtio-blk/net/snd/input + Aero WDDM GPU).
4. Boot again. Plug and Play will bind the newly-present devices to the staged driver packages.

### What it does

1. Creates a state directory at `C:\AeroGuestTools\`.
2. Installs Aero signing certificate(s) from `certs\` (`*.cer`, `*.crt`, `*.p7b`) into:
   - `Root` (Trusted Root Certification Authorities)
   - `TrustedPublisher` (Trusted Publishers)
3. On **Windows 7 x64**, optionally enables **test signing** (`bcdedit /set testsigning on`).
4. Stages all driver packages found under:
   - `drivers\x86\` (on Win7 x86)
   - `drivers\amd64\` (on Win7 x64)
5. Adds boot-critical registry plumbing for switching the boot disk from AHCI → virtio-blk:
   - `HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase\PCI#VEN_xxxx&DEV_yyyy...`
   - `HKLM\SYSTEM\CurrentControlSet\Services\<storage-service>\Start=0` etc.

### Output

`setup.cmd` writes:

- `C:\AeroGuestTools\install.log`
- `C:\AeroGuestTools\installed-driver-packages.txt` (for best-effort uninstall)
- `C:\AeroGuestTools\installed-certs.txt` (for best-effort uninstall)

### Usage

Run as Administrator:

```bat
setup.cmd
```

Optional flags:

- `setup.cmd /stageonly`  
  Only adds driver packages to the Driver Store (does not attempt immediate installs).
- `setup.cmd /testsigning`  
  On x64, enable test signing without prompting.
- `setup.cmd /notestsigning`  
  On x64, do not change the test-signing state.
- `setup.cmd /nointegritychecks`  
  On x64, disable signature enforcement entirely (**not recommended**; use only if test signing is not sufficient).
- `setup.cmd /noreboot`  
  Do not prompt for shutdown/reboot at the end.

## `uninstall.cmd`

Run as Administrator:

```bat
uninstall.cmd
```

This is best-effort and may fail to remove in-use drivers (especially if the VM is currently using virtio-blk as the boot disk).

Output:

- `C:\AeroGuestTools\uninstall.log`

## Troubleshooting / Recovery (storage)

If the VM fails to boot after switching to **virtio-blk**:

1. Switch storage back to **AHCI** and boot Windows again.
2. Re-run `setup.cmd` as Administrator.
3. Review `C:\AeroGuestTools\install.log`.

## `verify.cmd` / `verify.ps1`

Offline diagnostics + verification for Aero Windows 7 drivers.

### Output

Running `verify.cmd` writes:

- `C:\AeroGuestTools\report.json` (machine-readable)
- `C:\AeroGuestTools\report.txt` (human-readable)

### Usage

1. Boot Windows 7 SP1 (x86 or x64).
2. Install Aero drivers from the Guest Tools media.
3. Run **as Administrator**:
   - Right-click `verify.cmd` → **Run as administrator**
4. Open `C:\AeroGuestTools\report.txt`.

Optional parameters (PowerShell-style; forwarded by `verify.cmd`):

```bat
verify.cmd -PingTarget 192.168.0.1
verify.cmd -PlayTestSound
```

If `-PingTarget` is not provided, the script will attempt to ping the default gateway (if present).

### Checks performed

Each check produces a `PASS` / `WARN` / `FAIL` result:

- **OS + arch**: version, build, service pack.
- **KB3033929 (SHA-256 signatures)**: detects whether the hotfix is installed (relevant for SHA-256-signed driver packages on Win7).
- **Certificate store**: verifies that Guest Tools certificate(s) (from `certs\`, if present) are installed into **Local Machine**:
  - Trusted Root Certification Authorities (**Root**)
  - Trusted Publishers (**TrustedPublisher**)
- **Driver packages**: `pnputil -e` output with a heuristic filter for Aero/virtio-related packages.
- **Bound devices**: WMI `Win32_PnPEntity` enumeration (and optional `devcon.exe` if present alongside the script), including best-effort signed driver details via `Win32_PnPSignedDriver` (INF name, version, signer, etc).
- **Device binding by class**: best-effort checks that look for virtio/Aero devices and whether they are error-free in Device Manager:
  - Storage (virtio-blk)
  - Network (virtio-net)
  - Graphics (Aero GPU / virtio-gpu)
  - Audio (virtio-snd)
  - Input (virtio-input)
- **virtio-blk storage service**: best-effort probe for the configured storage driver service (see `config\devices.cmd`; e.g. `aeroviostor`) with state + Start type.
- **virtio-blk boot-critical registry**: validates `HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase\...` mappings for the configured virtio-blk HWIDs (helps prevent `0x7B` when switching the boot disk from AHCI → virtio-blk).
- **Signature mode**: parses `bcdedit` for `testsigning` and `nointegritychecks`.
- **Smoke tests**:
  - Disk I/O: create + read a temp file.
  - Network: detect IP-enabled adapters; optionally ping a target.
  - Audio: verify a `Win32_SoundDevice` exists; optionally play a `.wav`.
  - Input: report `Win32_Keyboard` and `Win32_PointingDevice` presence.

### Notes

- `bcdedit` and some driver/service information may be incomplete without Administrator privileges.
- The tool is designed to work on **Windows 7 SP1** without any external dependencies beyond built-in Windows components.
