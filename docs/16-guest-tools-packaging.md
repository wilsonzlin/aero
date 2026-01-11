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
      # Any auxiliary files referenced by the INF (e.g. WdfCoInstaller*.dll,
      # helper DLL/EXEs, manifests, etc) are preserved, with exclusions below.
  amd64/   (or `x64/` on input; the packaged output uses `amd64/`)
    <driver-name>/
      *.inf
      *.sys
      *.cat
      # Same as above.
```

#### Driver file inclusion / exclusions

When copying each `drivers/<arch>/<driver-name>/...` directory into the Guest Tools ISO/zip, the packager includes **all files** by default to keep Windows PnP driver packages installable (especially KMDF-based ones that require `WdfCoInstaller*.dll`).

It applies a small exclusion policy:

- Skipped by default (to avoid bloating artifacts with build outputs):
  - debug symbols: `*.pdb`, `*.ipdb`, `*.iobj`
  - build metadata: `*.obj`, `*.lib`
  - source / project files: `*.c`, `*.cpp`, `*.h`, `*.sln`, `*.vcxproj`, etc
- **Refused (hard error)** to avoid leaking secrets:
  - private key material: `*.pfx`, `*.pvk`, `*.snk`

Per-driver overrides can be configured in the packaging spec via `allow_extensions` and `allow_path_regexes`.

### Guest Tools scripts / certs

The packager expects:

```
guest-tools/
  setup.cmd
  uninstall.cmd
  verify.cmd
  verify.ps1
  README.md
  THIRD_PARTY_NOTICES.md
  licenses/ (optional)
  config/
    devices.cmd
  certs/ (optional when signing_policy=none)
    *.{cer,crt,p7b}   (optional)
```

## Outputs

The tool produces the following in the output directory:

- `aero-guest-tools.iso`
- `aero-guest-tools.zip`
- `manifest.json` (renamed to `aero-guest-tools.manifest.json` by CI wrapper scripts)

The packaged media also includes `THIRD_PARTY_NOTICES.md` at the ISO/zip root.

The ISO/zip root layout matches what `guest-tools/setup.cmd` expects:

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
  certs/ (optional)
    *.{cer,crt,p7b}   (optional)
  licenses/ (optional)
  drivers/
    x86/
      ...
    amd64/
      ...
```

## Running locally

### CI-style flow (signed drivers → Guest Tools ISO/zip)

The repository ships a CI-friendly wrapper script that consumes the signed driver packages
produced by the Win7 driver pipeline and emits Guest Tools media into `out/artifacts/`:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/install-wdk.ps1
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/build-drivers.ps1 -ToolchainJson out/toolchain.json -RequireDrivers
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/make-catalogs.ps1 -ToolchainJson out/toolchain.json
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/sign-drivers.ps1 -ToolchainJson out/toolchain.json

# Optional (also produces the standalone driver bundle ZIP/ISO/VHD artifacts):
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/package-drivers.ps1

# Guest Tools media (ISO + zip) built from the signed packages in out/packages/:
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/package-guest-tools.ps1
```

Notes:

- `ci/package-guest-tools.ps1` stages drivers into the packager input layout (`x86/<driver>/...`, `amd64/<driver>/...`),
  copies `guest-tools/`, and (when `-SigningPolicy` is not `none`) replaces any placeholder certs with `out/certs/aero-test.cer`
  so the resulting ISO matches the signed driver catalogs.
- `-InputRoot` defaults to `out/packages/`, but you can also point it at an extracted `*-bundle.zip` produced by
  `ci/package-drivers.ps1` (or the `*-bundle.zip` file itself; the wrapper can auto-extract and auto-detect the layout).
- Determinism is controlled by `SOURCE_DATE_EPOCH` (or `-SourceDateEpoch`). When unset, the wrapper uses the HEAD commit
  timestamp to keep outputs stable for a given commit.

Outputs:

- `out/artifacts/aero-guest-tools.iso`
- `out/artifacts/aero-guest-tools.zip`
- `out/artifacts/aero-guest-tools.manifest.json`

### Direct packager invocation (advanced)

```bash
cd tools/packaging/aero_packager

SOURCE_DATE_EPOCH=0 cargo run --release -- \
  --drivers-dir /path/to/drivers \
  --guest-tools-dir /path/to/guest-tools \
  --spec /path/to/spec.json \
  --out-dir /path/to/out \
  --version 1.2.3 \
  --build-id local \
  --signing-policy testsigning
```

Signing policy notes:

- `--signing-policy testsigning` (default): media is intended for test-signed/custom-signed drivers; `setup.cmd` may prompt to enable Test Signing on Win7 x64 and the packager requires at least one public cert under `certs/`.
- `--signing-policy nointegritychecks`: media may prompt to enable `nointegritychecks` on Win7 x64; also requires at least one public cert under `certs/`.
- `--signing-policy none`: media is intended for WHQL/production-signed drivers; `setup.cmd` does **not** prompt to enable Test Mode / `nointegritychecks` by default and the packager allows packaging with **zero** cert files.

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
- each included driver's `.inf` files contain the expected hardware IDs (regex match, case-insensitive) if provided,
- each included driver's `.inf` files reference only files that exist in the packaged driver directory (best-effort; includes KMDF `WdfCoInstaller*.dll` checks).

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
- A lightweight consistency check that ensures `guest-tools/config/devices.cmd` (service name + HWIDs)
  stays in sync with the virtio-win packaging specs (`win7-virtio-win.json` + `win7-virtio-full.json`).

You can run the same check locally:

```bash
python tools/guest-tools/validate_config.py
python tools/guest-tools/validate_config.py --spec tools/packaging/specs/win7-virtio-full.json
```
