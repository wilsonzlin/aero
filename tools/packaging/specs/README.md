# Guest Tools packaging specs

These JSON files are consumed by `tools/packaging/aero_packager/` (`--spec`) to validate
driver artifacts before producing `aero-guest-tools.iso` / `aero-guest-tools.zip`.

Note: packaging specs validate and select **drivers** only. Optional guest-side utilities can be
provided under `guest-tools/tools/**` in the input tree; when present they are packaged into the
ISO/zip at `tools/**` using the same safety filtering rules as driver directories (for example
`*.pdb` is excluded by default and symlinks are refused).

`aero_packager` writes a `manifest.json` at the media root. In manifest schema v3+, the
manifest includes an `inputs` object recording the SHA-256 of the **exact spec JSON bytes**
and Windows device contract JSON bytes used for that packaging run. In-guest, this is
surfaced by `guest-tools/verify.ps1` as the `guest_tools_manifest_inputs` check (“Guest Tools
Packaging Inputs (manifest.json)”), which helps avoid confusion caused by spec/contract drift.

`drivers[].name` is the **driver directory name** under the packager input tree:
`drivers/{x86,amd64}/<name>/`.

## JSON Schema (`$schema`)

Specs may include an optional `$schema` field to enable editor/CI validation.

In this repo, `tools/packaging/specs/*.json` use:

```json
{
  "$schema": "../packaging-spec.schema.json"
}
```

The packager ignores `$schema` at runtime, but accepts it under strict spec parsing
(`deny_unknown_fields`).

## Optional spec flags

- `fail_on_unlisted_driver_dirs` (default: `false`)
  - When enabled, the packager enumerates the top-level directories under the input
    `drivers_dir/x86` and `drivers_dir/amd64|x64` and fails if it finds any directories that are
    **not** listed in `drivers[]` (case-insensitive, after legacy alias normalization like
    `aero-gpu` → `aerogpu`).
  - This catches a common CI misconfiguration: pointing `--drivers-dir` at the wrong root (extra
    driver folders are otherwise silently ignored by the spec).
  - Recommended: set this to `true` in CI-owned specs so packaging fails fast if the staged driver
    payload drifts.

Example:

```json
{
  "fail_on_unlisted_driver_dirs": true,
  "drivers": [
    { "name": "aerogpu", "required": true }
  ]
}
```

- `require_optional_drivers_on_all_arches` (default: `false`)
  - When enabled, any driver with `required=false` must be present for **both** x86 and amd64.
    (Otherwise, packaging fails.)
  - This prevents producing Guest Tools media where an optional driver is shipped for only one
    guest architecture, which can lead to confusing and inconsistent behaviour across x86/x64
    Windows guests.

All virtio HWID patterns in these specs are expected to follow the **Aero virtio contract v1**
(`AERO-W7-VIRTIO`, virtio-pci **modern-only**).

Canonical (revision-gated) contract-v1 HWIDs are:

- virtio-net: `PCI\VEN_1AF4&DEV_1041&REV_01`
- virtio-blk: `PCI\VEN_1AF4&DEV_1042&REV_01`
- virtio-input: `PCI\VEN_1AF4&DEV_1052&REV_01`
- virtio-snd: `PCI\VEN_1AF4&DEV_1059&REV_01`

Note: Windows also enumerates less-specific forms (without `&REV_01` / `SUBSYS_...`). Some packaging
specs validate only the `PCI\VEN_....&DEV_....` prefix for compatibility with virtio-win INFs; the
device contract still requires `REV_01`.

Note: these specs are JSON, so backslashes are escaped in the file itself. For example, the literal string in JSON is
`"PCI\\VEN_1AF4&DEV_1041&REV_01"` but it represents the Windows HWID `PCI\VEN_1AF4&DEV_1041&REV_01`.

To sanity-check drift between `guest-tools/config/devices.cmd` and these specs, run:

```bash
python3 tools/guest-tools/validate_config.py --spec tools/packaging/specs/win7-aero-guest-tools.json
```

## `win7-virtio-win.json`

Intended for packaging Guest Tools using a driver payload extracted from **virtio-win**.

- Requires: `viostor` (virtio-blk) + `netkvm` (virtio-net)
- Includes only the drivers listed in the spec.
  - By default, other driver directories present in the input are ignored.
  - To fail fast on unexpected input directories (recommended in CI), set
    `fail_on_unlisted_driver_dirs=true`.
- When packaging virtio-win drivers, `config/devices.cmd` must be generated from the **virtio-win**
  Windows device contract (`docs/windows-device-contract-virtio-win.json`) so:
  - `AERO_VIRTIO_*_SERVICE` matches the upstream INF `AddService` names (`viostor`, `netkvm`, ...)
  - `guest-tools/setup.cmd` can validate and pre-seed boot-critical storage without `/skipstorage`.
  `drivers/scripts/make-guest-tools-from-virtio-win.ps1` uses the virtio-win contract by default.

## `win7-virtio-full.json`

Same as `win7-virtio-win.json`, but also declares optional drivers:

- Optional: `vioinput` (virtio-input) + `viosnd` (virtio-snd)
- Validates **modern-only** virtio PCI IDs (`DEV_1052` / `DEV_1059`); transitional IDs are not accepted by the spec.

## `win7-aero-guest-tools.json`

Intended for packaging Guest Tools media from **Aero-built** (in-repo) Windows 7 driver packages
(the output of the Win7 driver CI pipeline under `out/packages/`).

This spec is the default used by `ci/package-guest-tools.ps1` and aims to match what
`guest-tools/setup.cmd` expects for a full "switch to virtio + Aero GPU" installation.

- Requires: `aerogpu` + `virtio-blk` + `virtio-net` + `virtio-input`
- Optional: `virtio-snd`

Contract notes:

- This spec is **AERO-W7-VIRTIO contract v1 strict**: virtio devices are expected to use the
  **modern-only** virtio-pci device IDs (`DEV_1042`/`DEV_1041`/`DEV_1052`) and contract v1
  uses PCI Revision ID `0x01` (`REV_01`).
- The virtio HWID regexes in this spec intentionally **do not** match transitional IDs
  (the virtio-pci `0x1000..0x103F` transitional device ID range; for example: `1AF4:1000` net,
  `1AF4:1001` blk, `1AF4:1011` input). This is deliberate: packaging should fail if a driver INF
  regresses back to transitional IDs and drops the modern IDs.

Notes:

- `aerogpu` is the canonical Guest Tools-facing directory name for the AeroGPU driver (source: `drivers/aerogpu/`).
  - Backwards compatibility:
    - The packager normalizes the legacy dashed name `aero-gpu` to `aerogpu` when loading specs.
    - The packager also accepts input driver directories named `drivers/<arch>/aero-gpu/` and emits the canonical `drivers/<arch>/aerogpu/` in outputs.
- The AeroGPU HWID validation is primarily sourced from `guest-tools/config/devices.cmd` (via
  `expected_hardware_ids_from_devices_cmd_var`) so the packager stays in sync with the in-guest
  installer configuration. Any additional `expected_hardware_ids` regexes in the spec are also
  enforced.
  The default device contract (and generated `guest-tools/config/devices.cmd`) targets the canonical, versioned AeroGPU
  device (`PCI\VEN_A3A0&DEV_0001`); the deprecated legacy bring-up device is intentionally excluded from the default
  Guest Tools media. If you need it for compatibility/bring-up, install using the legacy INFs under
  `drivers/aerogpu/packaging/win7/legacy/` and build the emulator with the legacy device model enabled
  (feature `emulator/aerogpu-legacy`).

## `win7-aerogpu-only.json`

Intended for **development/debug** Guest Tools media that ships **only the AeroGPU driver** and
omits all virtio storage/network/input/audio drivers.

This is useful when you want to iterate on the AeroGPU driver without doing a full "switch the VM
to virtio" flow (and without shipping the boot-critical virtio-blk driver).

- Requires: `aerogpu`
- Does **not** include: `virtio-blk`, `virtio-net`, `virtio-input`, `virtio-snd`

In-guest, run the installer with:

```bat
setup.cmd /skipstorage
```

so `setup.cmd` does not attempt virtio-blk pre-seeding (which would fail without the storage
driver present).

## `win7-signed.json`

Intended for packaging Guest Tools from **CI-built, signed driver packages** (`out/packages` + `out/certs`)
without pinning stable hardware IDs yet.

- Requires: `aerogpu` + `virtio-blk` + `virtio-net` + `virtio-input`
- Optional: `virtio-snd`

Unlike `win7-aero-guest-tools.json`, this spec keeps `expected_hardware_ids` empty and derives
expected HWIDs from `guest-tools/config/devices.cmd` (via `expected_hardware_ids_from_devices_cmd_var`),
so CI can validate driver binding stays in sync with the device contract without hardcoding regexes here.

This spec is used by the Win7 driver CI/release workflows when packaging Guest Tools from the signed
packages produced by `ci/make-catalogs.ps1` + `ci/sign-drivers.ps1`.

To reproduce CI-style Guest Tools packaging locally (assuming you already have `out/packages/` + `out/certs/`):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/package-guest-tools.ps1 -SpecPath tools/packaging/specs/win7-signed.json
```

## `win7-aero-virtio.json`

Intended for packaging Guest Tools using Aero's in-tree clean-room Windows 7 virtio drivers.

- Requires: `aero_virtio_blk` (virtio-blk) + `aero_virtio_net` (virtio-net)
- Validates **modern-only** virtio PCI IDs (`DEV_1042` / `DEV_1041`); transitional IDs are not accepted by the spec.

This spec is used by `drivers/scripts/make-guest-tools-from-aero-virtio.ps1` by default.

## Wrapper script defaults (`make-guest-tools-from-virtio-win.ps1`)

`drivers/scripts/make-guest-tools-from-virtio-win.ps1` supports an explicit packaging profile:

- `-Profile full` (default): uses `win7-virtio-full.json` and extracts `viostor, netkvm, viosnd, vioinput` (optional `viosnd`/`vioinput` are best-effort)
  - Optional drivers are included only when present for **both** x86 and amd64; one-arch-only optional drivers are omitted (unless strict mode is used).
- `-Profile minimal`: uses `win7-virtio-win.json` and extracts `viostor, netkvm`

Advanced overrides:

- `-SpecPath` overrides the profile’s spec selection.
- `-Drivers` overrides the profile’s driver extraction list.
