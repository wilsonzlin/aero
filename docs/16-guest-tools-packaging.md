# 16 - Guest Tools Packaging (ISO + Zip)

This project distributes Windows drivers and helper scripts as a single, mountable **CD-ROM ISO** ("Aero Drivers / Guest Tools"), plus a `.zip` for manual extraction.

The packaging tool lives under:

- `tools/packaging/aero_packager/`

## CI/release packaging (from `out/packages` + `out/certs`)

Windows-driver CI produces:

- `out/packages/**` (staged + signed driver packages)
- `out/certs/aero-test.cer` (the *actual* signing certificate used for the driver catalogs)

To convert those CI outputs into the packager input layout and build the distributable Guest Tools media, use:

```powershell
pwsh -File ci/package-guest-tools.ps1
```

Convenience wrapper (same behaviour, located alongside other driver scripts):

```powershell
pwsh -File drivers/scripts/make-guest-tools-from-ci.ps1
```

The convenience wrapper forwards most arguments to `ci/package-guest-tools.ps1`, including:
`-SpecPath`, `-SigningPolicy`, and `-WindowsDeviceContractPath` (to override the device contract used
to generate the packaged `config/devices.cmd`).

By default this will:

- stage drivers into the layout expected by `aero_packager`:
  - `drivers/x86/<driver>/...`
  - `drivers/amd64/<driver>/...`
- map CI package roots (`out/packages/<driverRel>/{x86,x64}`) into stable Guest Tools-facing driver
  directory names (e.g. `drivers/aerogpu` → `aerogpu`, `windows7/virtio-blk` → `virtio-blk`, `windows7/virtio-net` → `virtio-net`)
- stage `guest-tools/` and normalize `certs/` based on signing policy:
  - `signing_policy=test`: inject `out/certs/aero-test.cer` (keeping `certs/README.md` if present)
  - `signing_policy=production|none`: do **not** inject certs (any existing `*.cer/*.crt/*.p7b` are stripped, leaving docs)
- produce:
  - `out/artifacts/aero-guest-tools.iso`
  - `out/artifacts/aero-guest-tools.zip`
  - `out/artifacts/manifest.json`
  - `out/artifacts/aero-guest-tools.manifest.json` (alias of `manifest.json` for CI/release asset naming)

### Spec selection (CI vs local)

`ci/package-guest-tools.ps1` uses a packaging spec (`-SpecPath`) to decide which driver
directories are allowed/required and which hardware IDs (HWIDs) to validate.

There are two common specs depending on whether you want to match CI/release behavior or
do stricter local validation:

| Spec | Typical use | Required drivers | Optional drivers | HWID validation |
|---|---|---|---|---|
| `tools/packaging/specs/win7-signed.json` | CI/release workflows (packaging from `out/packages` + `out/certs`) | `aerogpu`, `virtio-blk`, `virtio-net`, `virtio-input` | `virtio-snd` | Derives HWIDs from `devices.cmd` (no hardcoded regex list) |
| `tools/packaging/specs/win7-aero-guest-tools.json` | Local default (`ci/package-guest-tools.ps1` with no `-SpecPath`) | `aerogpu`, `virtio-blk`, `virtio-net`, `virtio-input` | `virtio-snd` | Stricter HWID validation (pins virtio HWIDs in the spec; AeroGPU HWIDs via `devices.cmd`) |

Note: “required/optional drivers” here refers to **packaging validation** (what the ISO/zip is expected to contain), not whether a given device is required at runtime (for example PS/2/HDA remain fallbacks for optional devices).

To reproduce CI packaging locally (assuming you already have `out/packages/` + `out/certs/`):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/package-guest-tools.ps1 -SpecPath tools/packaging/specs/win7-signed.json
```

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
  - build/link metadata: `*.obj`, `*.lib`, `*.exp`, `*.ilk`, `*.tlog`, `*.log`
  - source / project files: `*.c`, `*.cpp`, `*.h`, `*.sln`, `*.vcxproj`, etc
- **Refused (hard error)** to avoid leaking secrets:
  - private key material: `*.pfx`, `*.pvk`, `*.snk`, `*.key`, `*.pem` (case-insensitive)

The same private-key refusal applies to the `guest-tools/` input tree (e.g. `config/`, `certs/`, `licenses/`) as an extra safety net.

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
    README.md (optional)
    devices.cmd   (generated during packaging)
  certs/          (optional when signing_policy is production/none)
    README.md (optional but recommended)
    *.{cer,crt,p7b} (required for signing_policy=test; optional otherwise)
```

`config/devices.cmd` is generated during packaging from a Windows device contract JSON
(`--windows-device-contract` / `-WindowsDeviceContractPath`):

- `docs/windows-device-contract.json` (canonical; in-tree Aero driver service names like `aero_virtio_blk` / `aero_virtio_net`)
- `docs/windows-device-contract-virtio-win.json` (virtio-win; upstream service names like `viostor` / `netkvm` / `vioinput` / `viosnd`)

Virtio-win Guest Tools builds **must** use the virtio-win contract so `guest-tools/setup.cmd` can
validate the boot-critical storage INF `AddService` name and pre-seed registry state without
requiring `/skipstorage`.

Note:

- For **Aero** driver builds (in-tree virtio + AeroGPU), the contract’s `driver_service_name` values
  are expected to match the packaged INF `AddService` names (e.g. `aero_virtio_blk`).
- For **virtio-win** builds, pass the dedicated contract override
  `docs/windows-device-contract-virtio-win.json` to `ci/package-guest-tools.ps1 -WindowsDeviceContractPath`
  so the generated `devices.cmd` uses virtio-win service names (e.g. `viostor`, `netkvm`), while keeping
  Aero’s virtio PCI IDs/HWID patterns for boot-critical `CriticalDeviceDatabase` seeding.
  Do **not** edit the canonical `docs/windows-device-contract.json` to virtio-win names.

## Outputs

The tool produces the following in the output directory:

- `aero-guest-tools.iso`
- `aero-guest-tools.zip`
- `manifest.json`
- `aero-guest-tools.manifest.json` (alias of `manifest.json`)

The CI wrapper script (`ci/package-guest-tools.ps1`) also writes a copy of the manifest as
`aero-guest-tools.manifest.json` to avoid collisions when packaging into a shared artifact directory.

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
  certs/
    README.md (optional)
    *.{cer,crt,p7b} (optional)
  licenses/ (optional)
  drivers/
    x86/
      ...
    amd64/
      ...
```

When building Guest Tools from an upstream `virtio-win.iso` via
`drivers/scripts/make-guest-tools-from-virtio-win.ps1`, the wrapper will also attempt to
populate `licenses/virtio-win/` with upstream license/notice files (when present) and
include `driver-pack-manifest.json` for virtio-win ISO provenance.

## Packaging in-tree aero virtio drivers (aero_virtio_blk + aero_virtio_net)

If you built Aero's in-tree Windows 7 virtio drivers (`drivers/windows7/virtio-{blk,net}`) and
have a packager-style driver directory:

```
<DriverOutDir>/
  x86/aero_virtio_blk/*.{inf,sys,cat}
  x86/aero_virtio_net/*.{inf,sys,cat}
  amd64/aero_virtio_blk/*.{inf,sys,cat}   # (or x64/ instead of amd64/)
  amd64/aero_virtio_net/*.{inf,sys,cat}
```

You can build Guest Tools media directly using:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-aero-virtio.ps1 `
  -DriverOutDir C:\path\to\driver-out `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local
```

Signing policy notes:

- Default is `-SigningPolicy test` (`signing_policy=test` in `manifest.json`).
- Use `-SigningPolicy production` (or `none`) to omit `certs/*.{cer,crt,p7b}` from the packaged media (and avoid
  Test Signing prompts) when shipping WHQL/production-signed drivers.
- Use `-CertPath` to inject a different test root certificate into the staged `certs/` directory.

This uses the modern-only validation spec:

- `tools/packaging/specs/win7-aero-virtio.json`

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
  copies `guest-tools/`, and (when `-SigningPolicy` resolves to `test`) injects `out/certs/aero-test.cer` into `certs/`
  so the resulting ISO matches the signed driver catalogs.
- The wrapper drives inclusion/validation via a packager spec (`-SpecPath`):
  - Local default: `tools/packaging/specs/win7-aero-guest-tools.json` (stricter HWID validation).
  - CI/release workflows: pass `tools/packaging/specs/win7-signed.json` (derives HWID patterns from `devices.cmd`; no hardcoded regex list).
- `config/devices.cmd` is generated by `aero_packager` from a Windows device contract JSON
  (default: `docs/windows-device-contract.json`; override via `ci/package-guest-tools.ps1 -WindowsDeviceContractPath`).
  Ensure the contract’s `virtio-blk.driver_service_name` matches the packaged driver’s INF `AddService` name so
  `setup.cmd` boot-critical pre-seeding aligns with the driver packages that are shipped.
  - In-tree Aero builds use `docs/windows-device-contract.json`.
  - Virtio-win Guest Tools builds must use `docs/windows-device-contract-virtio-win.json` so the packaged `devices.cmd`
    matches virtio-win’s `AddService` names (e.g. `viostor` / `netkvm`).
- `-InputRoot` defaults to `out/packages/`, but you can also point it at an extracted `*-bundle.zip` produced by
  `ci/package-drivers.ps1` (or the `*-bundle.zip` file itself; the wrapper can auto-extract and auto-detect the layout).
- Determinism is controlled by `SOURCE_DATE_EPOCH` (or `-SourceDateEpoch`). When unset, the wrapper uses the HEAD commit
  timestamp to keep outputs stable for a given commit.

Outputs:

- `out/artifacts/aero-guest-tools.iso`
- `out/artifacts/aero-guest-tools.zip`
- `out/artifacts/manifest.json`
- `out/artifacts/aero-guest-tools.manifest.json`

### Direct packager invocation (advanced)

```bash
cd tools/packaging/aero_packager

SOURCE_DATE_EPOCH=0 cargo run --release --locked -- \
  --drivers-dir /path/to/drivers \
  --guest-tools-dir /path/to/guest-tools \
  --spec /path/to/spec.json \
  --out-dir /path/to/out \
  --version 1.2.3 \
  --build-id local \
  --signing-policy test
```

### Signing policy (test vs production vs none)

Guest Tools media includes a `manifest.json` that describes **signing expectations**:

- `signing_policy`: `test` | `production` | `none`
- `certs_required`: derived from `signing_policy` (currently `true` only for `test`)

This is consumed by `guest-tools/setup.cmd` / `verify.ps1` so they can behave appropriately:

- `test`: packager requires at least one cert file under `guest-tools/certs/` and setup will
  prompt to enable Test Signing on Windows 7 x64 (or enable it automatically under `/force`).
- `production`: packager allows `guest-tools/certs/` to contain only docs (or be empty) and setup
  will not prompt to enable Test Signing by default.
- `none`: same as `production` for certificate/Test Signing behavior (intended for development).

The packager default is `--signing-policy test` to preserve historical behavior.

For back-compat, the packager also accepts legacy aliases:

- `testsigning` / `test-signing` → `test`
- `nointegritychecks` → `none`

### Building Guest Tools from an upstream virtio-win ISO (Win7 virtio drivers)

If you want the packaged Guest Tools ISO/zip to include the **virtio-win** drivers (viostor + NetKVM at minimum), use the wrapper script:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local
```

On Linux/macOS, you can run the same PowerShell wrapper under PowerShell 7 (`pwsh`):
it will automatically fall back to the cross-platform extractor when `Mount-DiskImage`
is unavailable (or fails to mount).

```bash
pwsh drivers/scripts/make-guest-tools-from-virtio-win.ps1 \
  -VirtioWinIso virtio-win.iso \
  -OutDir ./dist/guest-tools \
  -Version 0.0.0 \
  -BuildId local
```

Alternatively, you can extract first and pass `-VirtioWinRoot`:

```bash
python3 tools/virtio-win/extract.py --virtio-win-iso virtio-win.iso --out-root /tmp/virtio-win-root
pwsh drivers/scripts/make-guest-tools-from-virtio-win.ps1 -VirtioWinRoot /tmp/virtio-win-root -OutDir ./dist/guest-tools
```

Convenience wrapper (Linux/macOS): `bash ./drivers/scripts/make-guest-tools-from-virtio-win.sh`.

Note: The virtio-win wrapper uses `docs/windows-device-contract-virtio-win.json` as the contract template and emits a
temporary contract override (service names derived from the extracted driver INFs) when calling the CI packager wrapper,
so the generated `config/devices.cmd` matches upstream virtio-win driver service names (`viostor`, `netkvm`, `vioinput`,
`viosnd`). This keeps `setup.cmd` boot-critical storage pre-seeding aligned with the packaged drivers.

`-Profile` controls both:

- the default driver set extracted from virtio-win (unless overridden by `-Drivers`), and
- the default packaging spec (unless overridden by `-SpecPath`).

Profiles (defaults):

- `full` (default):
  - `-Drivers @('viostor','netkvm','viosnd','vioinput')`
  - `-SpecPath tools/packaging/specs/win7-virtio-full.json` (optional `viosnd`/`vioinput` are best-effort)
- `minimal`:
  - `-Drivers @('viostor','netkvm')`
  - `-SpecPath tools/packaging/specs/win7-virtio-win.json`

To build storage+network-only Guest Tools media (no optional audio/input drivers), use `-Profile minimal`:

```powershell
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -Profile minimal `
  -OutDir .\dist\guest-tools `
  -Version 0.0.0 `
  -BuildId local
```

For advanced/custom validation, you can override the profile’s spec selection via `-SpecPath`.

Signing policy:

- The wrapper defaults to `-SigningPolicy none` (appropriate for WHQL/production-signed virtio-win drivers).
- If you are packaging test-signed/custom-signed drivers, override it (e.g. `-SigningPolicy test`).
  - Legacy alias accepted: `testsigning` (maps to `test`).

Notes:

- `-SpecPath` overrides the profile’s default spec selection.
- `-Drivers` overrides the profile’s default driver list.
- The wrapper generates a device contract override (based on `docs/windows-device-contract-virtio-win.json`) so `setup.cmd`
  boot-critical storage pre-seeding uses the correct virtio-win storage `AddService` name (typically `viostor`).
- `-Profile full` does **not** enable `-StrictOptional` by default; missing `viosnd`/`vioinput` should remain best-effort unless strict mode is requested.

## Validation: required drivers + hardware IDs

Before producing any output, the packager verifies that:

- the output includes **only** driver directories listed in the packaging spec (prevents accidentally shipping stray/incomplete driver folders),
- each **required** driver is present for both `x86` and `amd64` (missing required drivers are fatal),
- each included driver (required + optional that are present) contains at least one `.inf`, `.sys`, and `.cat`,
- each included driver's `.inf` files contain the expected hardware IDs (regex match, case-insensitive) if provided,
- each included driver's `.inf` files reference only files that exist in the packaged driver directory (best-effort; validates common directives like `CopyFiles=`, `CopyINF=`, `SourceDisksFiles*`, and includes KMDF `WdfCoInstaller*.dll` sanity checks).

These checks are driven by a small JSON spec passed via `--spec`.

For Aero packaging profiles, the virtio HWID patterns are expected to match the **Aero virtio
contract v1** (virtio-pci **modern-only**). Transitional device IDs (the older virtio-pci
`0x1000..` device ID range) are intentionally not accepted in the in-repo specs.

### Spec schema: required + optional drivers

The current schema uses a unified `drivers` list where each entry declares whether it is required:

```json
{
  "drivers": [
    {"name": "viostor", "required": true, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_1042"]},
    {"name": "netkvm", "required": true, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_1041"]},
    {"name": "vioinput", "required": false, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_1052"]},
    {"name": "viosnd", "required": false, "expected_hardware_ids": ["PCI\\\\VEN_1AF4&DEV_1059"]}
  ]
}
```

If an optional driver is listed but missing from the input driver directory, the packager emits a warning and continues.

`expected_hardware_ids_from_devices_cmd_var` can be used instead of (or in addition to)
`expected_hardware_ids` to source expected HWIDs from `guest-tools/config/devices.cmd`. The packager
and config validator normalize these HWIDs down to the base `PCI\VEN_....&DEV_....` form before
validating that the driver INF matches.

Legacy specs using the older top-level `required_drivers` list are still accepted and treated as `required=true` entries.

## CI coverage (packager + config/spec drift)

GitHub Actions runs a dedicated workflow (`guest-tools-packager`) on PRs that touch Guest Tools
packaging inputs (`tools/packaging/**`, `guest-tools/**`, etc.). It covers:

- `cargo test --locked --manifest-path tools/packaging/aero_packager/Cargo.toml`
- A smoke packaging run that verifies the in-repo `guest-tools/` directory can be packaged (using the
  packager's dummy driver fixtures).
- A lightweight consistency check that ensures `guest-tools/config/devices.cmd` stays consistent with:
  - the Windows device contract (`docs/windows-device-contract.json`) for the boot-critical virtio-blk
    storage service name and the exact virtio-blk/virtio-net PCI hardware IDs that Guest Tools seeds, and
  - the in-repo packaging specs (HWID regexes):
  - `win7-signed.json`
  - `win7-virtio-win.json`
  - `win7-virtio-full.json`
  - `win7-aero-guest-tools.json`
  - `win7-aero-virtio.json`

Separately, CI also runs the `Windows device contract` workflow (`.github/workflows/windows-device-contract.yml`),
which runs the Rust validator:

```bash
cargo run -p device-contract-validator --locked
```

This provides an additional guardrail that the **Windows device contract manifests** remain consistent with:

- Guest Tools config (`guest-tools/config/devices.cmd`)
- Packager specs (`tools/packaging/specs/*.json`)
- In-tree Win7 driver INFs (`drivers/**`)
- Emulator PCI ID constants (best-effort static checks)

You can run the same check locally:

```bash
python tools/guest-tools/validate_config.py --spec tools/packaging/specs/win7-signed.json
python tools/guest-tools/validate_config.py
python tools/guest-tools/validate_config.py --spec tools/packaging/specs/win7-virtio-full.json
python tools/guest-tools/validate_config.py --spec tools/packaging/specs/win7-aero-guest-tools.json
python tools/guest-tools/validate_config.py --spec tools/packaging/specs/win7-aero-virtio.json

# To validate the virtio-win contract variant, you must point the validator at a devices.cmd
# generated from that contract (or extracted from a virtio-win Guest Tools ZIP). The in-repo
# guest-tools/config/devices.cmd is generated from the canonical Aero contract.
python scripts/generate-guest-tools-devices-cmd.py --contract docs/windows-device-contract-virtio-win.json --output /tmp/devices-virtio-win.cmd
python tools/guest-tools/validate_config.py --devices-cmd /tmp/devices-virtio-win.cmd --spec tools/packaging/specs/win7-virtio-win.json

# (Optional) If you are validating a devices.cmd copy that does not include a contract header,
# force the contract file explicitly:
# python tools/guest-tools/validate_config.py --devices-cmd /tmp/devices-virtio-win.cmd --windows-device-contract docs/windows-device-contract-virtio-win.json --spec tools/packaging/specs/win7-virtio-win.json
```
