# Aero Guest Tools (Windows 7)

This directory contains utilities intended to be shipped on the **Aero Guest Tools ISO** and run **inside** a Windows 7 SP1 VM.

## `verify.cmd` / `verify.ps1`

Offline diagnostics + verification for Aero Windows 7 drivers.

### Output

Running `verify.cmd` writes:

- `C:\AeroGuestTools\report.json` (machine-readable)
- `C:\AeroGuestTools\report.txt` (human-readable)

### Usage

1. Boot Windows 7 SP1 (x86 or x64).
2. Install Aero drivers from the Guest Tools ISO.
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
- **Driver packages**: `pnputil -e` output with a heuristic filter for Aero/virtio-related packages.
- **Bound devices**: WMI `Win32_PnPEntity` enumeration (and optional `devcon.exe` if present alongside the script).
- **virtio-blk storage service**: best-effort probe for a storage driver service (e.g. `viostor`) with state + Start type.
- **Signature mode**: parses `bcdedit` for `testsigning` and `nointegritychecks`.
- **Smoke tests**:
  - Disk I/O: create + read a temp file.
  - Network: detect IP-enabled adapters; optionally ping a target.
  - Audio: verify a `Win32_SoundDevice` exists; optionally play a `.wav`.
  - Input: report `Win32_Keyboard` and `Win32_PointingDevice` presence.

### Notes

- `bcdedit` and some driver/service information may be incomplete without Administrator privileges.
- The tool is designed to work on **Windows 7 SP1** without any external dependencies beyond built-in Windows components.

