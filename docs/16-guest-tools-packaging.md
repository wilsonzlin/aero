# 16 - Guest Tools Packaging (ISO + Zip)

This project distributes Windows drivers and helper scripts as a single, mountable **CD-ROM ISO** ("Aero Drivers / Guest Tools"), plus a `.zip` for manual extraction.

The packaging tool lives under:

- `tools/packaging/aero_packager/`

## Inputs

### Driver artifacts

The packager expects a directory containing two architecture subdirectories:

```
drivers/
  x86/
    <driver-name>/
      *.inf
      *.sys
      *.cat
  amd64/   (or `x64/` on input; the packaged output uses `amd64/`)
    <driver-name>/
      *.inf
      *.sys
      *.cat
```

### Guest Tools scripts / certs

The packager expects:

```
guest-tools/
  setup.cmd
  uninstall.cmd
  verify.cmd (optional)
  verify.ps1 (optional, but required if verify.cmd is present)
  README.md
  THIRD_PARTY_NOTICES.md
  licenses/ (optional)
  config/
    devices.cmd
  certs/
    *.{cer,crt,p7b}
```

## Outputs

The tool produces the following in the output directory:

- `aero-guest-tools.iso`
- `aero-guest-tools.zip`
- `manifest.json`

The packaged media also includes `THIRD_PARTY_NOTICES.md` at the ISO/zip root.

The ISO/zip root layout matches what `guest-tools/setup.cmd` expects (and may include optional verification scripts):

```
/
  setup.cmd
  uninstall.cmd
  verify.cmd
  verify.ps1
  README.md
  THIRD_PARTY_NOTICES.md
  manifest.json
  config/
    devices.cmd
  certs/
    *.{cer,crt,p7b}
  licenses/ (optional)
  drivers/
    x86/
      ...
    amd64/
      ...
```

## Running locally

```bash
cd tools/packaging/aero_packager

SOURCE_DATE_EPOCH=0 cargo run --release -- \
  --drivers-dir /path/to/drivers \
  --guest-tools-dir /path/to/guest-tools \
  --spec /path/to/spec.json \
  --out-dir /path/to/out \
  --version 1.2.3 \
  --build-id local
```

### Building Guest Tools from an upstream virtio-win ISO (Win7 virtio drivers)

If you want the packaged Guest Tools ISO/zip to include the **virtio-win** drivers (viostor + NetKVM at minimum), use the wrapper script:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local
```

This uses the in-repo "minimal" spec:

- `tools/packaging/specs/win7-virtio-win.json` (required: `viostor` + `netkvm`)

To also include optional virtio drivers (if present in the input), use:

- `tools/packaging/specs/win7-virtio-full.json` (optional: `vioinput` + `viosnd`)

## Validation: required drivers + hardware IDs

Before producing any output, the packager verifies that:

- the output includes **only** driver directories listed in the packaging spec (prevents accidentally shipping stray/incomplete driver folders),
- each **required** driver is present for both `x86` and `amd64` (missing required drivers are fatal),
- each included driver (required + optional that are present) contains at least one `.inf`, `.sys`, and `.cat`,
- each included driver's `.inf` files contain the expected hardware IDs (regex match, case-insensitive) if provided.

These checks are driven by a small JSON spec passed via `--spec`.

### Spec schema: required + optional drivers

The current schema uses a unified `drivers` list where each entry declares whether it is required:

```json
{
  "drivers": [
    {"name": "viostor", "required": true, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_(1001|1042)"]},
    {"name": "netkvm", "required": true, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_(1000|1041)"]},
    {"name": "vioinput", "required": false, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_(1011|1052)"]},
    {"name": "viosnd", "required": false, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_(1018|1059)"]}
  ]
}
```

If an optional driver is listed but missing from the input driver directory, the packager emits a warning and continues.

Legacy specs using the older top-level `required_drivers` list are still accepted and treated as `required=true` entries.

## CI coverage (packager + config/spec drift)

GitHub Actions runs a dedicated workflow (`guest-tools-packager`) on PRs that touch Guest Tools
packaging inputs (`tools/packaging/**`, `guest-tools/**`, etc.). It covers:

- `cargo test --manifest-path tools/packaging/aero_packager/Cargo.toml`
- A lightweight consistency check that ensures `guest-tools/config/devices.cmd` HWIDs stay in sync
  with `tools/packaging/specs/win7-virtio-win.json`.

You can run the same check locally:

```bash
python tools/guest-tools/validate_config.py
```
