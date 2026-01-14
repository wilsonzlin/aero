# Driver ISO builder

This tool builds an ISO image that can be mounted inside Aero as a virtual CD-ROM.

The ISO root includes `THIRD_PARTY_NOTICES.md` for redistribution attribution.

If the input driver root includes a `manifest.json` (for example from
`drivers/scripts/make-driver-pack.ps1`), it is copied into the ISO root as-is.
You can additionally include `drivers/virtio/manifest.json` via `--include-manifest`
(it is written as `virtio-manifest.json` if `manifest.json` already exists).

## Dependencies

The builder supports multiple ISO authoring backends (see `--backend`):

- **Rust (preferred, deterministic):** uses the in-tree `aero_iso` tool via `cargo`.
  - Selected by default when `cargo` is available (`--backend auto`).
  - Use `--backend rust` to force this backend.
  - Determinism is controlled by `--source-date-epoch` / `SOURCE_DATE_EPOCH` (defaults to `0`).
- **External tooling (fallback):** shells out to an ISO authoring tool:

- Linux: `xorriso` (preferred) or `genisoimage` / `mkisofs`
- Windows (optional): `oscdimg` (from the Windows ADK)
- Windows (fallback): IMAPI (built-in) when no third-party tooling is available

## Build

From repo root:

```bash
python3 tools/driver-iso/build.py \
  --drivers-root drivers/virtio/prebuilt \
  --output dist/aero-virtio-win7-drivers.iso
```

To build a deterministic ISO (recommended):

```bash
python3 tools/driver-iso/build.py \
  --backend rust \
  --source-date-epoch 0 \
  --drivers-root drivers/virtio/prebuilt \
  --output dist/aero-virtio-win7-drivers.iso
```

The builder can also consume the output of `drivers/scripts/make-driver-pack.ps1` (which creates
`drivers/out/aero-win7-driver-pack/` containing `win7/x86/...` and `win7/amd64/...`):

```bash
python3 tools/driver-iso/build.py \
  --drivers-root drivers/out/aero-win7-driver-pack \
  --output dist/aero-virtio-win7-drivers.iso
```

To build a demo ISO from placeholders:

```bash
python3 tools/driver-iso/build.py \
  --drivers-root drivers/virtio/sample \
  --output dist/aero-virtio-win7-drivers-sample.iso
```

## Verify

The verifier lists ISO contents using one of:

- `cargo` (preferred): the in-tree `aero_iso_ls` tool
- `pycdlib` (`python3 -m pip install pycdlib`)
- `xorriso`

```bash
python3 tools/driver-iso/verify_iso.py \
  --iso dist/aero-virtio-win7-drivers-sample.iso
```
