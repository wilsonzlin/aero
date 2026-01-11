# Packaging: Aero Drivers / Guest Tools

This directory contains the tooling used to produce the distributable **Aero Drivers / Guest Tools** media:

- `aero-guest-tools.iso` (mountable CD-ROM image)
- `aero-guest-tools.zip` (manual extraction)
- `manifest.json` (SHA-256 hashes + build metadata)

The packager is implemented as a small, self-contained Rust CLI under `tools/packaging/aero_packager/`.

Redistribution note: the packaged ISO/zip includes `THIRD_PARTY_NOTICES.md` at the
media root, and may include additional third-party license texts under `licenses/`
when present in the input directory.

## Quickstart

```bash
cd tools/packaging/aero_packager

# Example:
#   drivers/ contains:
#     x86/<driver>/**   (PnP driver package; at minimum: .inf/.sys/.cat; may also include UMD/coinstaller .dll files)
#     amd64/<driver>/** (or x64/ on input; the packaged output uses amd64/)
#       Note: the packager includes driver directories recursively and applies a small
#       exclusion policy for build outputs (e.g. .pdb/.obj); see docs/16-guest-tools-packaging.md.
#   guest-tools/ contains:
#     setup.cmd
#     uninstall.cmd
#     verify.cmd
#     verify.ps1
#     README.md
#     THIRD_PARTY_NOTICES.md
#     config/devices.cmd
#     certs/*.{cer,crt,p7b}   (optional when --signing-policy none)
#     licenses/** (optional; third-party license texts / attribution files)
#
# spec.json declares which drivers to include (required + optional) and expected HWID regexes.

cargo run --release -- \
  --drivers-dir /path/to/drivers \
  --guest-tools-dir /path/to/guest-tools \
  --spec /path/to/spec.json \
  --out-dir /path/to/out \
  --version 1.2.3 \
  --build-id ci-123 \
  --signing-policy testsigning
```

Use `--signing-policy none` (or `AERO_GUEST_TOOLS_SIGNING_POLICY=none`) to build Guest Tools
media for WHQL/production-signed drivers without requiring any certificate files.

## Building Guest Tools from `virtio-win.iso` (Win7 virtio drivers)

If you want Guest Tools to include the upstream virtio drivers (`viostor`, `netkvm`, etc.), use:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -Profile full `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local
```

Profiles:

- `-Profile full` (default): uses `tools/packaging/specs/win7-virtio-full.json`
- `-Profile minimal`: uses `tools/packaging/specs/win7-virtio-win.json`

For advanced/custom validation, you can override the profile’s spec selection via `-SpecPath`.

When built from a virtio-win ISO/root, the wrapper script also attempts to
propagate upstream license/notice files into the packaged outputs under:

- `licenses/virtio-win/` (including `driver-pack-manifest.json` for provenance)

## Determinism / reproducible builds

The packager aims to be reproducible: **same inputs → bit-identical outputs**.

Timestamps in the ISO/zip are controlled by `SOURCE_DATE_EPOCH` (or `--source-date-epoch`).

```bash
SOURCE_DATE_EPOCH=0 cargo run --release -- ...
```

## Spec format

The packager uses a small JSON spec to validate and sanity-check the driver artifacts before packaging.

See `tools/packaging/aero_packager/testdata/spec.json` for a minimal example.

The current schema uses a unified `drivers` list:

```json
{
  "drivers": [
    {"name": "viostor", "required": true, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_(1001|1042)"]},
    {"name": "viosnd", "required": false, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_(1018|1059)"]}
  ]
}
```

Legacy specs using `required_drivers` are still accepted and treated as `required=true` entries.
