# Aero Guest Tools (Windows 7)

This directory is intended to be shipped as the root of the **Aero Guest Tools ISO** and run **inside** a Windows 7 SP1 VM.

It provides:

- `setup.cmd`: offline certificate + driver installer
- `uninstall.cmd`: best-effort cleanup
- `verify.cmd` / `verify.ps1`: offline diagnostics/verification
- `THIRD_PARTY_NOTICES.md`: third-party attribution/redistribution notices for packaged components
- `licenses\`: third-party license/notice files (when present in the packaged media)
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
2. Installs Aero signing certificate(s) from `certs\` (`*.cer`, `*.crt`, `*.p7b`) **if any are present** into:
   - `Root` (Trusted Root Certification Authorities)
   - `TrustedPublisher` (Trusted Publishers)
3. On **Windows 7 x64**, may enable a driver-signing boot policy depending on `manifest.json`:
   - `testsigning` (`bcdedit /set testsigning on`)
   - `nointegritychecks` (`bcdedit /set nointegritychecks on`) (**not recommended**)
   - `none` (do not prompt or change boot policy; for WHQL/production-signed drivers)
4. Stages all driver packages found under:
   - `drivers\x86\` (on Win7 x86)
   - `drivers\amd64\` (on Win7 x64)
5. Adds boot-critical registry plumbing for switching the boot disk from AHCI → virtio-blk:
   - Validates that the configured storage service name (`AERO_VIRTIO_BLK_SERVICE`) exists in at least one staged driver INF as an `AddService` name (fails fast if not).
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

### Signing policy (manifest.json)

Guest Tools media built by `tools/packaging/aero_packager` includes a `manifest.json` file that carries a `signing_policy` field.

`setup.cmd` uses this policy to decide whether to prompt for enabling Test Mode / signature bypass on **Windows 7 x64**:

- `none`: do not prompt or change `bcdedit` settings (for WHQL/production-signed drivers).
- `testsigning`: prompt to enable Test Signing (default for dev/test builds).
- `nointegritychecks`: prompt to disable signature enforcement (**not recommended**).

Explicit command-line flags override the manifest.

Optional flags:

- `setup.cmd /force` (or `setup.cmd /quiet`)  
  Fully non-interactive/unattended mode:
   - implies `/noreboot` (never prompts reboot/shutdown)
   - on x64, applies the effective `signing_policy` without prompting (e.g. enables Test Signing or `nointegritychecks`)
     - use `/forcesigningpolicy:none` to keep boot policy unchanged
- `setup.cmd /stageonly`  
  Only adds driver packages to the Driver Store (does not attempt immediate installs).
- `setup.cmd /testsigning`  
  On x64, enable test signing without prompting.
- `setup.cmd /forcetestsigning`  
  Alias of `/testsigning` (overrides `manifest.json`).
- `setup.cmd /notestsigning`  
  On x64, do not change the test-signing state.
- `setup.cmd /nointegritychecks`  
  On x64, disable signature enforcement entirely (**not recommended**; use only if test signing is not sufficient).
- `setup.cmd /forcenointegritychecks`  
  Alias of `/nointegritychecks` (overrides `manifest.json`).
- `setup.cmd /forcesigningpolicy:none|testsigning|nointegritychecks`  
  Override the `signing_policy` read from `manifest.json` (if present).
- `setup.cmd /noreboot`  
  Do not prompt for shutdown/reboot at the end.

Exit codes (for automation):

- `0`: success
- `10`: Administrator privileges required
- `11`: driver directory missing (`drivers\\<arch>\\`)
- `13`: `AERO_VIRTIO_BLK_SERVICE` does not match any `AddService` name in the staged driver INFs
## Building Guest Tools media for WHQL / production-signed drivers

If you are shipping only WHQL/production-signed drivers (for example from `virtio-win`), you can build Guest Tools media that:

- contains **no** `certs\*.cer/*.crt/*.p7b`, and
- does **not** prompt to enable Test Mode / `nointegritychecks` by default.

When building artifacts with `tools/packaging/aero_packager`, set:

- `--signing-policy none` (or `AERO_GUEST_TOOLS_SIGNING_POLICY=none`)

and ensure the input Guest Tools `certs\` directory contains **zero** certificate files.

## `uninstall.cmd`

Run as Administrator:

```bat
uninstall.cmd
```

This is best-effort and may fail to remove in-use drivers (especially if the VM is currently using virtio-blk as the boot disk).

Optional flags:

- `uninstall.cmd /force`  
  Skips the interactive "Continue with uninstall?" prompt (for automation). In `/force` mode, the script also skips the interactive prompts for disabling `testsigning` / `nointegritychecks` (leaves boot configuration unchanged).
- `uninstall.cmd /quiet`  
  Fully non-interactive alias for `/force /noreboot`.
- `uninstall.cmd /noreboot`  
  Do not prompt for shutdown/reboot at the end.

If the current Guest Tools media `manifest.json` has `signing_policy=none`, `uninstall.cmd` also defaults to **not** prompting about Test Signing / `nointegritychecks` changes.

Output:

- `C:\AeroGuestTools\uninstall.log`

Exit codes:

- `0`: success
- `10`: Administrator privileges required

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
- **Guest Tools media integrity (manifest.json)**:
  - records Guest Tools version/build metadata (if present),
  - verifies that the files listed in the manifest exist and match their SHA-256 hashes (detects corrupted/incomplete ISO/zip copies).
- **Guest Tools setup state**: reads `C:\AeroGuestTools\install.log` and related state files from `setup.cmd` (if present) to show what Guest Tools staged/changed.
- **Guest Tools config**: reads `config\devices.cmd` to understand expected virtio/Aero PCI IDs and storage service name (used by `setup.cmd` and some verify checks).
- **Packaged drivers (media INFs)**: parses `.inf` files under `drivers\<arch>\...` on the Guest Tools media to extract:
  - Provider
  - `DriverVer`
  - best-effort HWID patterns (used for later correlation)
- **KB3033929 (SHA-256 signatures)**: detects whether the hotfix is installed (relevant for SHA-256-signed driver packages on Win7).
- **Certificate store**: verifies that Guest Tools certificate(s) (from `certs\`, e.g. `*.cer`, `*.crt`, `*.p7b`) are installed into **Local Machine**:
  - Trusted Root Certification Authorities (**Root**)
  - Trusted Publishers (**TrustedPublisher**)
- **Driver packages**: `pnputil -e` output with a heuristic filter for Aero/virtio-related packages (and, if available, cross-checks packages recorded by `setup.cmd`).
- **Bound devices**: WMI `Win32_PnPEntity` enumeration (and optional `devcon.exe` if present alongside the script), including best-effort signed driver details via `Win32_PnPSignedDriver` (INF name, version, signer, etc).
- **Installed driver binding correlation (media vs system)**: correlates the running system’s device bindings (`Win32_PnPSignedDriver`) against the packaged media driver INFs to show which devices are using drivers present on this Guest Tools media vs “unknown/mismatched” drivers.
- **Device binding by class**: best-effort checks that look for virtio/Aero devices and whether they are error-free in Device Manager:
  - Storage (virtio-blk)
  - Network (virtio-net)
  - Graphics (Aero GPU / virtio-gpu)
  - Audio (virtio-snd)
  - Input (virtio-input)
- **virtio-blk storage service**: best-effort probe for the configured storage driver service (see `config\devices.cmd`; e.g. `viostor`) with state + Start type.
- **virtio-blk boot-critical registry**: validates `HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase\...` mappings for the configured virtio-blk HWIDs (helps prevent `0x7B` when switching the boot disk from AHCI → virtio-blk).
- **Signature mode**: parses `bcdedit` for `testsigning` and `nointegritychecks`.
- **Smoke tests**:
  - Disk I/O: create + read a temp file.
  - Network: detect IP-enabled adapters; optionally ping a target.
  - Graphics: verify a `Win32_VideoController` exists and report basic adapter/driver state.
  - Audio: verify a `Win32_SoundDevice` exists; optionally play a `.wav`.
  - Input: report `Win32_Keyboard` and `Win32_PointingDevice` presence.

### Notes

- `bcdedit` and some driver/service information may be incomplete without Administrator privileges.
- The tool is designed to work on **Windows 7 SP1** without any external dependencies beyond built-in Windows components.

### `report.json` structured summary sections

In addition to the per-check `checks` object, newer versions of `verify.ps1` include these top-level structured sections:

- `media_integrity`
- `packaged_drivers_summary`
- `installed_driver_binding_summary`
