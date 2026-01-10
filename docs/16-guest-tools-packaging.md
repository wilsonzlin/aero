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

The ISO/zip root layout matches what `guest-tools/setup.cmd` expects (and may include optional verification scripts):

```
/
  setup.cmd
  uninstall.cmd
  verify.cmd
  verify.ps1
  README.md
  manifest.json
  config/
    devices.cmd
  certs/
    *.{cer,crt,p7b}
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

## Validation: required drivers + hardware IDs

Before producing any output, the packager verifies that:

- each required driver is present for both `x86` and `amd64`,
- each required driver contains at least one `.inf`, `.sys`, and `.cat`,
- each required driver's `.inf` files contain the expected hardware IDs (regex match).

These checks are driven by a small JSON spec passed via `--spec`.
