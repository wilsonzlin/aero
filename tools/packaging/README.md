# Packaging: Aero Drivers / Guest Tools

This directory contains the tooling used to produce the distributable **Aero Drivers / Guest Tools** media:

- `aero-guest-tools.iso` (mountable CD-ROM image)
- `aero-guest-tools.zip` (manual extraction)
- `manifest.json` (SHA-256 hashes + build metadata)

The packager is implemented as a small, self-contained Rust CLI under `tools/packaging/aero_packager/`.

## Quickstart

```bash
cd tools/packaging/aero_packager

# Example:
#   drivers/ contains:
#     x86/<driver>/*.inf|*.sys|*.cat
#     amd64/<driver>/*.inf|*.sys|*.cat   (or x64/ on input; the packaged output uses amd64/)
#   guest-tools/ contains:
#     setup.cmd
#     uninstall.cmd
#     verify.cmd (optional, but included if present)
#     verify.ps1 (optional, but required if verify.cmd is present)
#     README.md
#     config/devices.cmd
#     certs/*.{cer,crt,p7b}
#
# spec.json declares which drivers are required + expected HWIDs.

cargo run --release -- \
  --drivers-dir /path/to/drivers \
  --guest-tools-dir /path/to/guest-tools \
  --spec /path/to/spec.json \
  --out-dir /path/to/out \
  --version 1.2.3 \
  --build-id ci-123
```

## Building Guest Tools from `virtio-win.iso` (Win7 virtio drivers)

If you want Guest Tools to include the upstream virtio drivers (`viostor`, `netkvm`, etc.), use:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local
```

This uses the validation spec at:

- `tools/packaging/specs/win7-virtio-win.json`

## Determinism / reproducible builds

The packager aims to be reproducible: **same inputs → bit-identical outputs**.

Timestamps in the ISO/zip are controlled by `SOURCE_DATE_EPOCH` (or `--source-date-epoch`).

```bash
SOURCE_DATE_EPOCH=0 cargo run --release -- ...
```

## Spec format

The packager uses a small JSON spec to validate and sanity-check the driver artifacts before packaging.

See `tools/packaging/aero_packager/testdata/spec.json` for a minimal example.
