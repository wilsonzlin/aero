# Aero Guest Tools (Windows 7)

This directory is intended to be shipped as the root of the **Aero Guest Tools ISO** and run **inside** a Windows 7 SP1 VM.

It provides:

- `setup.cmd`: offline certificate + driver installer
- `uninstall.cmd`: best-effort cleanup
- `verify.cmd` / `verify.ps1`: offline diagnostics/verification
- `THIRD_PARTY_NOTICES.md`: third-party attribution/redistribution notices for packaged components
- `licenses\`: third-party license/notice files (when present in the packaged media)
- `certs\`: optional public certificate(s) needed to validate driver signatures (for test-signed/custom-signed drivers; installed only when `signing_policy=test` unless overridden; should be empty for WHQL/production-signed media)
- `drivers\`: PnP driver packages for x86 + amd64 (at minimum `.inf/.sys/.cat`, plus any INF-referenced payload files such as UMD/coinstaller `*.dll`)
- `config\`: expected device IDs (PCI VEN/DEV pairs) used for boot-critical pre-seeding (generated; see below)
- `tools\`: optional guest-side helper utilities (debugging, selftests, diagnostics) shipped alongside the media (see below)

## Regenerating `config/devices.cmd` (repo developers)

`guest-tools/config/devices.cmd` is **generated** from the Windows device contract manifest:

- `docs/windows-device-contract.json`

To update device HWIDs / service names, edit the JSON manifest and regenerate:

```bash
python3 scripts/generate-guest-tools-devices-cmd.py
```

To check for drift without modifying files (CI-style):

```bash
python3 scripts/ci/gen-guest-tools-devices-cmd.py --check
```

CI fails if `devices.cmd` is out of sync with the manifest.

For a broader drift check across the Windows device contract + Guest Tools + packaging specs + INFs + emulator IDs:

```bash
cargo run -p device-contract-validator --locked
```

## Optional `tools\` directory (extra utilities)

Guest Tools media can optionally ship additional **guest-side utilities** (debugging, selftests, diagnostics) under `tools\...` without placing them inside any driver package directory.

The AeroGPU debug/control utility (`aerogpu_dbgctl.exe`) is shipped alongside the AeroGPU driver package under `drivers\...`. It is shipped as a single **x86** binary and copied into both the x86 and amd64 driver trees (on amd64 it runs via **WOW64**):

- `drivers\<arch>\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`
- `drivers\<arch>\aerogpu\tools\win7_dbgctl\README.md` (tool documentation)

Some Guest Tools builds also include a convenience copy in the optional top-level `tools\` payload (for example `tools\aerogpu_dbgctl.exe` or `tools\<arch>\aerogpu_dbgctl.exe`). If both are present, prefer the driver-packaged copy under `drivers\<arch>\...` since it is versioned alongside the installed driver package.

Example (run directly from a mounted Guest Tools ISO/zip; if your mount letter differs from `X:`, replace it):

```bat
:: Win7 x64:
X:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --status
:: Win7 x86:
X:\drivers\x86\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --status
```

If the input Guest Tools tree contains a `tools\` directory, `tools/packaging/aero_packager` will package it recursively into the ISO/zip under `tools\...`.

Recommended layout:

```
tools\
  x86\
    <32-bit tools>
  amd64\
    <64-bit tools>
```

Notes:

- Common host/build artifacts (for example `*.pdb`, `*.obj`, `*.ilk`, `*.lib`) are excluded by default to keep the packaged outputs small and deterministic.
- Hidden files/directories and OS metadata files (for example `.DS_Store`, `__MACOSX`, `Thumbs.db`, `desktop.ini`) are ignored.
- Private key material (`*.pfx`, `*.pvk`, `*.snk`, `*.key`, `*.pem`) is refused (packaging fails).

## `setup.cmd`

Designed for the standard flow:

1. Install Windows 7 SP1 using “safe” emulated devices first (AHCI/e1000/HDA/PS2/VGA).
2. Boot Windows and run `setup.cmd` **as Administrator**.
3. Power off/reboot and switch the VM devices to Aero virtio devices (virtio-blk/net/snd/input + Aero WDDM GPU).
4. Boot again. Plug and Play will bind the newly-present devices to the staged driver packages.

### What it does

1. Creates a state directory at `C:\AeroGuestTools\`.
2. For `manifest.json` `signing_policy=test` (default for dev/test builds), installs Aero signing certificate(s) from `certs\` (`*.cer`, `*.crt`, `*.p7b`) into:
   - `Root` (Trusted Root Certification Authorities)
   - `TrustedPublisher` (Trusted Publishers)
   
   For `signing_policy=production|none`, `setup.cmd` skips certificate installation by default (even if cert files are present) and logs a warning; production/WHQL Guest Tools media should not ship any `certs\*.cer`, `certs\*.crt`, or `certs\*.p7b`.
3. On **Windows 7 x64**, may prompt to enable **Test Signing** depending on `manifest.json` `signing_policy`:
   - `test`: test-signed/custom-signed drivers are expected; `setup.cmd` may prompt to enable Test Signing.
   - `production` / `none`: production/WHQL-signed drivers are expected; `setup.cmd` does **not** prompt by default.
   - `nointegritychecks` is supported as an **explicit** override flag (`setup.cmd /nointegritychecks`) but is not enabled automatically by policy (not recommended).
4. Stages all driver packages found under:
   - `drivers\x86\` (on Win7 x86)
   - `drivers\amd64\` (on Win7 x64)
5. Adds boot-critical registry plumbing for switching the boot disk from AHCI → virtio-blk (**required before switching storage**):
   - Validates that the configured storage service name (`AERO_VIRTIO_BLK_SERVICE`) exists in at least one packaged driver INF (under `drivers\<arch>\...`) as an `AddService` name (fails fast if not).
   - `HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase\PCI#VEN_xxxx&DEV_yyyy...`
   - `HKLM\SYSTEM\CurrentControlSet\Services\<storage-service>\Start=0` etc.

   This step can be skipped with `/skipstorage` (alias: `/skip-storage`) for GPU-only installs / development builds that do not ship the virtio-blk driver.

### Output

`setup.cmd` writes:

- `C:\AeroGuestTools\install.log`
- `C:\AeroGuestTools\installed-driver-packages.txt` (for best-effort uninstall)
- `C:\AeroGuestTools\installed-certs.txt` (for best-effort uninstall)
- `C:\AeroGuestTools\installed-media.txt` (records which Guest Tools ISO/zip build ran `setup.cmd`; used by `verify.ps1` to detect “mixed media” issues)
- `C:\AeroGuestTools\storage-preseed.skipped.txt` (only when `/skipstorage` is used)

### Usage

Run as Administrator:

```bat
setup.cmd
```

Validation-only (no system changes; does not require Administrator):

```bat
setup.cmd /check
```

### Signing policy (manifest.json)

Guest Tools media built by `tools/packaging/aero_packager` includes a `manifest.json` file that carries a `signing_policy` field.

`setup.cmd` uses this policy to decide whether to prompt for enabling Test Mode / signature bypass on **Windows 7 x64**:

- `test`: prompt to enable Test Signing (default for dev/test builds).
- `production`: do not prompt or change `bcdedit` settings (for WHQL/production-signed drivers).
- `none`: same as `production` for certificate/Test Signing behavior (development use).

Legacy values in older `manifest.json` are supported and normalized:

- `testsigning` / `test-signing` → `test`
- `nointegritychecks` / `no-integrity-checks` → `none`
- `prod` / `whql` → `production`

Explicit command-line flags override the manifest.

Optional flags:

- `setup.cmd /check` (alias: `/validate`)  
  Non-destructive validation mode intended for CI/automation and cautious users. Performs
  media-level checks (drivers directory, `config\devices.cmd`, `manifest.json` signing policy,
  certificate payload presence rules, and virtio-blk `AddService` name validation unless
  `/skipstorage` is provided).  
  Writes logs under `%TEMP%\AeroGuestToolsCheck\install.log`.
- `setup.cmd /force` (or `setup.cmd /quiet`)  
  Fully non-interactive/unattended mode:
   - implies `/noreboot` (never prompts reboot/shutdown)
   - on x64, implies `/testsigning` only when `signing_policy=test` (unless `/notestsigning` is provided)
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
- `setup.cmd /forcesigningpolicy:none|test|production`  
  Override the `signing_policy` read from `manifest.json` (if present).
  - legacy aliases accepted: `testsigning`→`test`, `nointegritychecks`→`none`
- `setup.cmd /installcerts`  
  Force certificate installation from `certs\` even when `signing_policy=production|none` (advanced; not recommended).
- `setup.cmd /verify-media`  
  Verify Guest Tools media integrity by hashing all files listed in `manifest.json` before making any system changes.  
  If any file is missing or has a SHA-256 mismatch, setup exits with code `14` and prints remediation guidance:
  "replace the Guest Tools ISO/zip with a fresh copy; do not mix driver folders across versions".
  Tip: combine with validation-only mode to avoid installation side effects: `setup.cmd /check /verify-media`.
- `setup.cmd /noreboot`  
  Do not prompt for shutdown/reboot at the end.
- `setup.cmd /skipstorage` (alias: `/skip-storage`)  
  Skip boot-critical virtio-blk storage pre-seeding. This is intended for partial Guest Tools payloads (for example AeroGPU-only development builds). The canonical GPU-only media payload is built using `tools/packaging/specs/win7-aerogpu-only.json`.  
  **Do not switch the boot disk from AHCI → virtio-blk** unless you later re-run `setup.cmd` without `/skipstorage` using Guest Tools media that includes the virtio-blk driver; otherwise Windows may BSOD with `0x0000007B INACCESSIBLE_BOOT_DEVICE`.

Exit codes (for automation):

- `0`: success
- `10`: Administrator privileges required
- `11`: driver directory missing (`drivers\\<arch>\\`)
- `12`: required certificate file(s) missing under `certs\\` (when `signing_policy=test`)
- `13`: `AERO_VIRTIO_BLK_SERVICE` does not match any `AddService` name in the packaged driver INFs (`drivers\\<arch>\\...`)
- `14`: Guest Tools media integrity check failed (`/verify-media`)

## Building Guest Tools media for WHQL / production-signed drivers

If you are shipping only WHQL/production-signed drivers (for example from `virtio-win`), you can build Guest Tools media that:

- contains **no** `certs\*.cer`, `certs\*.crt`, or `certs\*.p7b`, and
- does **not** prompt to enable Test Mode / `nointegritychecks` by default.

When building artifacts with `tools/packaging/aero_packager`, set:

- `--signing-policy production` (or `none`)

and ensure the input Guest Tools `certs\` directory contains **zero** certificate files.

Newer `setup.cmd` versions also refuse to import certificates when `signing_policy=production|none` (unless `/installcerts` is explicitly provided), but production/WHQL media should still ship with an empty `certs\` directory to avoid warnings and reduce the risk of accidentally trusting development certificates.

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
- `uninstall.cmd /cleanupstorage` (alias: `/cleanup-storage`)  
  **DANGEROUS (boot-critical registry cleanup).** Reverts the boot-critical virtio-blk pre-seeding performed by `setup.cmd` by:
  - setting `HKLM\SYSTEM\CurrentControlSet\Services\<AERO_VIRTIO_BLK_SERVICE>\Start` to `3` (DEMAND_START), and
  - deleting `HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase\PCI#...` keys for the configured virtio-blk HWIDs (including the `&CC_010000` / `&CC_0100` variants).

  Only run this when you are **NOT** currently booting from virtio-blk (for example after switching the boot disk back to AHCI).  
  Running this while booting from virtio-blk may cause Windows to fail to boot (for example `0x0000007B INACCESSIBLE_BOOT_DEVICE`).

  In interactive mode (default), `uninstall.cmd` prompts before touching the registry.  
  In `/force` or `/quiet` mode, `/cleanupstorage` is ignored unless `/cleanupstorageforce` (alias: `/cleanup-storage-force`) is also provided.
- `uninstall.cmd /noreboot`  
  Do not prompt for shutdown/reboot at the end.

`uninstall.cmd` only prompts about Test Signing / `nointegritychecks` if `setup.cmd` previously enabled them (marker files under `C:\AeroGuestTools\`). For `signing_policy=production|none` media, these markers are not created by default.

For diagnostics, `uninstall.cmd` also logs the Guest Tools manifest metadata (version/build_id) and `signing_policy`. It searches for `manifest.json` at the media root (one directory above the script directory) and falls back to `manifest.json` next to the script (same behavior as `setup.cmd`).  
If `C:\AeroGuestTools\installed-media.txt` exists, `uninstall.cmd` prints it and warns if it does not match the currently-running Guest Tools media (helps detect “mixed media” / wrong ISO cases).

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

If you installed Guest Tools with `setup.cmd /skipstorage` (check for `C:\AeroGuestTools\storage-preseed.skipped.txt`), boot-critical virtio-blk pre-seeding was intentionally skipped. Re-run `setup.cmd` **without** `/skipstorage` using Guest Tools media that includes the virtio-blk driver before attempting to boot from virtio-blk.

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
verify.cmd -RunDbgctl
verify.cmd -RunDbgctlSelftest
```

If `-PingTarget` is not provided, the script will attempt to ping the default gateway (if present).

`-RunDbgctl` is **off by default**. When enabled and an **AeroGPU** device is detected, `verify.ps1` will attempt to run:

- `drivers\<arch>\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --status --timeout-ms 2000`

`-RunDbgctlSelftest` is also **off by default**. When enabled, `verify.ps1` will additionally attempt to run:

- `drivers\<arch>\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --selftest --timeout-ms 2000`

Note: selftest may return `GPU_BUSY` on active desktops; treat it as a best-effort diagnostic.

The captured stdout/stderr/exit code is embedded into:

- `C:\AeroGuestTools\report.txt`
- `C:\AeroGuestTools\report.json` (`aerogpu.dbgctl` and `aerogpu.dbgctl_selftest`)

### Checks performed

Each check produces a `PASS` / `WARN` / `FAIL` result:

- **OS + arch**: version, build, service pack.
- **Guest Tools media integrity (manifest.json)**:
  - records Guest Tools version/build metadata (if present),
  - verifies that the files listed in the manifest exist and match their SHA-256 hashes (detects corrupted/incomplete ISO/zip copies).
- **Guest Tools setup state**: reads `C:\AeroGuestTools\install.log` and related state files from `setup.cmd` (if present) to show what Guest Tools staged/changed, including `installed-media.txt` to detect installed-vs-current media mismatches.
- **Guest Tools config**: reads `config\devices.cmd` to understand expected virtio/Aero PCI IDs and storage service name (used by `setup.cmd` and some verify checks).
- **Packaged drivers (media INFs)**: parses `.inf` files under `drivers\<arch>\...` on the Guest Tools media to extract:
  - Provider
  - `DriverVer`
  - best-effort HWID patterns (used for later correlation)
- **Clock sanity**: warns if the guest date/time is obviously wrong (incorrect clock can break signature verification).
- **SHA-2 hotfix prerequisites (KB3033929 / KB4474419 / KB4490628)**: detects whether these updates are installed (relevant for SHA-2/SHA-256-signed driver packages on Win7).
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
- **AeroGPU registry configuration (segment budget override)**: when an AeroGPU device is detected, reads:
  - `HKR\Parameters\NonLocalMemorySizeMB` (REG_DWORD) — if missing, the driver default is used.
  - Appears in `report.txt` under **Device Binding: Graphics** details and in `report.json` under `aerogpu.non_local_memory_size_mb` + `aerogpu.non_local_memory_size_mb_note`.
- **AeroGPU D3D9 UMD DLL placement**: when an AeroGPU device is detected, verifies that the expected D3D9 UMD DLL(s) exist.
  - On Win7 x64 this includes the WOW64 D3D9 UMD under `C:\Windows\SysWOW64\` (required for 32-bit D3D9 apps).
- **AeroGPU D3D10/11 UMD DLL placement (optional)**: if any AeroGPU D3D10/11 UMD DLLs are detected, verifies that the expected D3D10/11 UMD DLL(s) exist.
  - On Win7 x64 this includes the WOW64 D3D10/11 UMD under `C:\Windows\SysWOW64\` (required for 32-bit D3D10/D3D11 apps when using the DX11-capable driver package).
- **virtio-blk storage service**: best-effort probe for the configured storage driver service (see `config\devices.cmd`; e.g. `aero_virtio_blk`, or `viostor` when packaging virtio-win) with state + Start type.
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
- `aerogpu` (including `non_local_memory_size_mb` and, when enabled, `dbgctl` / `dbgctl_selftest`)
