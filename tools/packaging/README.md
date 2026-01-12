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

# (Alternative) Run from the repo root without `cd`:
# cargo run --release --locked --manifest-path tools/packaging/aero_packager/Cargo.toml -- \
#   --drivers-dir /path/to/drivers \
#   --guest-tools-dir /path/to/guest-tools \
#   --spec /path/to/spec.json \
#   --out-dir /path/to/out \
#   --version 1.2.3 \
#   --build-id ci-123 \
#   --signing-policy test

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
#     certs/*.{cer,crt,p7b}   (required for --signing-policy test; optional for production/none)
#     licenses/** (optional; third-party license texts / attribution files)
#
# spec.json declares which drivers to include (required + optional) and expected HWID regexes.

cargo run --release --locked -- \
  --drivers-dir /path/to/drivers \
  --guest-tools-dir /path/to/guest-tools \
  --spec /path/to/spec.json \
  --out-dir /path/to/out \
  --version 1.2.3 \
  --build-id ci-123 \
  --signing-policy test
```

Use `--signing-policy production` (or `none`) to build Guest Tools media for
WHQL/production-signed drivers without requiring any certificate files.

## Windows device contract variants (`config/devices.cmd`)

`aero_packager` generates `config/devices.cmd` from a machine-readable Windows device contract JSON
(`--windows-device-contract`).

This repo maintains two variants:

- `docs/windows-device-contract.json` (canonical, in-tree Aero driver service names like `aero_virtio_blk` / `aero_virtio_net`)
- `docs/windows-device-contract-virtio-win.json` (upstream virtio-win service names like `viostor` / `netkvm`)

Virtio-win Guest Tools builds **must** use the virtio-win contract so `guest-tools/setup.cmd` can
validate the boot-critical storage INF (`AddService = viostor, ...`) and pre-seed registry state
without requiring `/skipstorage`.

## Building Guest Tools from `virtio-win.iso` (Win7 virtio drivers, optional / compatibility)

Official CI/release Guest Tools media is built from signed in-tree driver packages under `out/packages/**`
(see the section below). The virtio-win flow is an alternative path for packaging upstream drivers.

If you want Guest Tools to include the upstream virtio drivers (`viostor`, `netkvm`, etc.), use:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local
```

By default this wrapper uses `-Profile full` (includes optional Win7 audio/input drivers when present; best-effort).
To build storage+network-only Guest Tools media, use `-Profile minimal`.

This wrapper uses `docs/windows-device-contract-virtio-win.json` by default so the packaged
`config/devices.cmd` matches the upstream virtio-win INF service names (required for storage
pre-seeding).

On Linux/macOS you can run the same wrapper under PowerShell 7 (`pwsh`). When `Mount-DiskImage`
is unavailable (or fails to mount), it automatically falls back to the cross-platform extractor:

```bash
pwsh drivers/scripts/make-guest-tools-from-virtio-win.ps1 \
  -VirtioWinIso virtio-win.iso \
  -OutDir ./dist/guest-tools \
  -Version 0.0.0 \
  -BuildId local
```

Convenience wrapper (Linux/macOS): `bash ./drivers/scripts/make-guest-tools-from-virtio-win.sh`.

Profiles (defaults):

- `-Profile full` (default): uses `tools/packaging/specs/win7-virtio-full.json` (optional `viosnd`/`vioinput` are best-effort)
- `-Profile minimal`: uses `tools/packaging/specs/win7-virtio-win.json`

For advanced/custom validation, you can override the profile’s spec selection via `-SpecPath`.
`-Drivers` also overrides the profile’s driver extraction list.

Signing policy notes:

- By default, the wrapper uses `-SigningPolicy none` (for WHQL/production-signed virtio-win drivers), so it does not require or inject any custom certs.
- You can override this via `-SigningPolicy test` when producing media for test-signed/custom-signed drivers.
  - Legacy alias accepted: `testsigning` (maps to `test`).

When built from a virtio-win ISO/root, the wrapper script also attempts to
propagate upstream license/notice files into the packaged outputs under:

- `licenses/virtio-win/` (including `driver-pack-manifest.json` for provenance)

## Building Guest Tools from CI-built driver packages (`out/packages/**`)

When the Win7 driver CI pipeline stages signed driver packages under `out/packages/**`, you can
produce the Guest Tools ISO/zip from those artifacts using:

- `ci/package-guest-tools.ps1`
  - Local default (when `-SpecPath` is omitted): `tools/packaging/specs/win7-aero-guest-tools.json` (stricter HWID validation)
  - CI/release workflows: `tools/packaging/specs/win7-signed.json` (derives HWID patterns from `devices.cmd`; no hardcoded regex list)
  - Device contract (for generated `config/devices.cmd`): `-WindowsDeviceContractPath` (default: `docs/windows-device-contract.json`)

To reproduce CI packaging locally (assuming you already have `out/packages/` + `out/certs/`):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/package-guest-tools.ps1 -SpecPath tools/packaging/specs/win7-signed.json
```

## Building Guest Tools from in-tree aero virtio drivers (Win7 aero_virtio_blk + aero_virtio_net)

If you want Guest Tools to include Aero's in-tree Windows 7 virtio drivers (`aero_virtio_blk`, `aero_virtio_net`),
build them and point the wrapper at the resulting driver package directory:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-aero-virtio.ps1 `
  -DriverOutDir C:\path\to\driver-out `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local
```

By default this wrapper packages media intended for **test-signed** drivers (`signing_policy=test`).
To build Guest Tools for WHQL/production-signed drivers without shipping/installing any custom
certificates, pass `-SigningPolicy none`.

If your driver catalogs are signed with a different test root certificate, pass `-CertPath` to
replace the staged `guest-tools/certs/*` contents.

`-DriverOutDir` must contain driver packages for both architectures:

```
<DriverOutDir>/
  x86/aero_virtio_blk/*.{inf,sys,cat}
  x86/aero_virtio_net/*.{inf,sys,cat}
  amd64/aero_virtio_blk/*.{inf,sys,cat}   # (or x64/ instead of amd64/)
  amd64/aero_virtio_net/*.{inf,sys,cat}
```

This uses the validation spec at:

- `tools/packaging/specs/win7-aero-virtio.json`

## Determinism / reproducible builds

The packager aims to be reproducible: **same inputs → bit-identical outputs**.

Timestamps in the ISO/zip are controlled by `SOURCE_DATE_EPOCH` (or `--source-date-epoch`).

```bash
SOURCE_DATE_EPOCH=0 cargo run --release --locked -- ...
```

## Spec format

The packager uses a small JSON spec to validate and sanity-check the driver artifacts before packaging.

See `tools/packaging/aero_packager/testdata/spec.json` for a minimal example.

The current schema uses a unified `drivers` list:

```json
{
  "drivers": [
    {"name": "viostor", "required": true, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_1042"]},
    {"name": "viosnd", "required": false, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_1059"]}
  ]
}
```

Note: `AERO-W7-VIRTIO` v1 devices report PCI Revision `REV_01`, and some in-tree INFs (for example
virtio-snd) are revision-gated (they match `...&REV_01` only). Because `expected_hardware_ids`
entries are regex matches, using a prefix like `PCI\\VEN_1AF4&DEV_1059` will still match an INF
that contains `PCI\VEN_1AF4&DEV_1059&REV_01`. If you want to enforce revision gating at packaging
time, include `&REV_01` in the pattern.

Drivers can also set `expected_hardware_ids_from_devices_cmd_var` to source expected hardware IDs
from `guest-tools/config/devices.cmd`. Each token is normalized down to the base
`PCI\VEN_....&DEV_....` form and regex-escaped before being appended to `expected_hardware_ids`.

Legacy specs using `required_drivers` are still accepted and treated as `required=true` entries.
