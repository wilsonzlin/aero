# aero-win7-slipstream

End-to-end Windows 7 SP1 ISO patcher for Aero.

Given:

- a **user-supplied** Windows 7 SP1 ISO (32-bit or 64-bit), and
- an **Aero-supplied** driver pack (storage/network/GPU, etc), and optionally
- an **Aero root certificate** (for test-signed drivers),

this tool produces a new **bootable** ISO that:

- loads Aero drivers in Windows Setup (WinPE),
- stages them for the installed OS,
- enables the requested boot policy (`testsigning` or `nointegritychecks`),
- injects the Aero root certificate into offline images when needed.

No Windows files are shipped in this repository. The tool only transforms user-provided media.

## Build

```bash
cd tools/win7-slipstream
cargo build --release
```

Binary: `target/release/aero-win7-slipstream`

## CLI

```bash
aero-win7-slipstream deps
aero-win7-slipstream patch-iso --input Win7.iso --output Win7-Aero.iso --drivers ./aero-drivers --cert ./aero-root.cer
aero-win7-slipstream verify-iso --input Win7-Aero.iso
```

## Prerequisites

This tool is an orchestrator; it shells out to external tools for ISO/WIM/registry/BCD operations.

### Windows (recommended backend: `windows-dism`)

Required:

- **7-Zip** (`7z`) for ISO extraction
  - `winget install 7zip.7zip`
- **Windows ADK** “Deployment Tools” for `oscdimg.exe` (ISO rebuild)
- **DISM** (`dism.exe`) for mounting/patching WIMs
- **bcdedit.exe** for BCD editing
- **reg.exe** for offline registry edits

Fallbacks:

- If `oscdimg` is not available, install `xorriso` and the tool will fall back to it.

### Linux/macOS (backend: `cross-wimlib`)

Required:

- **p7zip** (`7z`) or **xorriso** (ISO extraction)
- **xorriso** (ISO rebuild)
- **wimlib-imagex** (mount/patch WIMs)
- **hivexregedit** (offline registry hive edits, including BCD hives)

Example install (Debian/Ubuntu):

```bash
sudo apt-get install p7zip-full xorriso wimtools libhivex-bin
```

Notes:

- `wimlib-imagex mount` uses FUSE on many systems. If mounting fails, ensure FUSE is available and permitted for your user.
- The cross backend focuses on unattend-based driver staging; true offline driver injection is only implemented for the Windows DISM backend.

## Examples

### Patch an ISO (auto arch detection, test signing)

```bash
cargo run --release -- \
  patch-iso \
  --input /path/to/Win7SP1.iso \
  --output /path/to/Win7SP1-Aero.iso \
  --drivers /path/to/aero-driver-pack \
  --signing-mode testsigning \
  --cert /path/to/aero-root.cer \
  --unattend drivers-only \
  --backend auto \
  --verbose
```

### Verify an output ISO

```bash
cargo run --release -- verify-iso --input /path/to/Win7SP1-Aero.iso --verbose
```

## Output layout

The patched ISO contains (at minimum):

- `AERO/DRIVERS/<arch>/...` – copied driver pack
- `autounattend.xml` (unless `--unattend none`) – points Setup to the driver directory via `%configsetroot%`
- `AERO/MANIFEST.json` – hashes + settings + list of patched paths

## Troubleshooting

- Run `aero-win7-slipstream deps` for a full dependency report and install hints.
- If ISO extraction fails on Windows, install 7-Zip; PowerShell ISO mount is only a fallback.
- If WIM mounting fails on Linux/macOS, verify:
  - `wimlib-imagex` is installed,
  - FUSE is available and usable by your user.
- If verification fails on BCD policy checks, ensure you built with a backend that can inspect hives (`bcdedit` on Windows or `hivexregedit` on Linux/macOS).

