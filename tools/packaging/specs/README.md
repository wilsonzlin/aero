# Guest Tools packaging specs

These JSON files are consumed by `tools/packaging/aero_packager/` (`--spec`) to validate
driver artifacts before producing `aero-guest-tools.iso` / `aero-guest-tools.zip`.

`drivers[].name` is the **driver directory name** under the packager input tree:
`drivers/{x86,amd64}/<name>/`.

All virtio HWID patterns in these specs are expected to follow the **Aero virtio contract v1**
(virtio-pci **modern-only**), i.e.:

- virtio-net: `PCI\VEN_1AF4&DEV_1041`
- virtio-blk: `PCI\VEN_1AF4&DEV_1042`
- virtio-input: `PCI\VEN_1AF4&DEV_1052`
- virtio-snd: `PCI\VEN_1AF4&DEV_1059`

Note: these specs are JSON, so backslashes are escaped in the file itself. For example, the literal string in JSON is
`"PCI\\VEN_1AF4&DEV_1041"` but it represents the Windows HWID `PCI\VEN_1AF4&DEV_1041`.

To sanity-check drift between `guest-tools/config/devices.cmd` and these specs, run:

```bash
python3 tools/guest-tools/validate_config.py --spec tools/packaging/specs/win7-aero-guest-tools.json
```

## `win7-virtio-win.json`

Intended for packaging Guest Tools using a driver payload extracted from **virtio-win**.

- Requires: `viostor` (virtio-blk) + `netkvm` (virtio-net)
- Includes only the drivers listed in the spec (other driver directories present in the input
  are ignored).
- Note: `aero_packager` will override `AERO_VIRTIO_BLK_SERVICE` / `AERO_VIRTIO_NET_SERVICE` in the
  packaged `config/devices.cmd` to match these virtio-win service names so `guest-tools/setup.cmd`
  can validate and pre-seed boot-critical storage correctly.

## `win7-virtio-full.json`

Same as `win7-virtio-win.json`, but also declares optional drivers:

- Optional: `vioinput` (virtio-input) + `viosnd` (virtio-snd)

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
  (the virtio-pci `0x1000..0x103F` transitional device ID range). This is deliberate: packaging
  should fail if a driver INF regresses back to transitional IDs and drops the modern IDs.

Notes:

- `aerogpu` is the canonical Guest Tools-facing directory name for the AeroGPU driver (source: `drivers/aerogpu/`).
  - Backwards compatibility: the packager normalizes the legacy dashed name `aero-gpu` to `aerogpu` when loading specs.
- The AeroGPU HWID validation is primarily sourced from `guest-tools/config/devices.cmd` (via
  `expected_hardware_ids_from_devices_cmd_var`) so the packager stays in sync with the in-guest
  installer configuration. Any additional `expected_hardware_ids` regexes in the spec are also
  enforced.
  The default device contract (and generated `guest-tools/config/devices.cmd`) targets the canonical, versioned AeroGPU
  device (`PCI\VEN_A3A0&DEV_0001`); the deprecated legacy bring-up device is intentionally excluded from the default
  Guest Tools media. If you need it for compatibility/bring-up, install using the legacy INFs under
  `drivers/aerogpu/packaging/win7/legacy/` and build the emulator with the legacy device model enabled
  (feature `emulator/aerogpu-legacy`).

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

- Requires: `aerovblk` (virtio-blk) + `aerovnet` (virtio-net)
- Validates **modern-only** virtio PCI IDs (`DEV_1042` / `DEV_1041`); transitional IDs are not accepted by the spec.

This spec is used by `drivers/scripts/make-guest-tools-from-aero-virtio.ps1` by default.

## Wrapper script defaults (`make-guest-tools-from-virtio-win.ps1`)

`drivers/scripts/make-guest-tools-from-virtio-win.ps1` supports an explicit packaging profile:

- `-Profile full` (default): uses `win7-virtio-full.json` and extracts `viostor, netkvm, viosnd, vioinput` (optional `viosnd`/`vioinput` are best-effort)
- `-Profile minimal`: uses `win7-virtio-win.json` and extracts `viostor, netkvm`

Advanced overrides:

- `-SpecPath` overrides the profile’s spec selection.
- `-Drivers` overrides the profile’s driver extraction list.
