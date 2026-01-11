# Driver ISO builder

This tool builds an ISO image that can be mounted inside Aero as a virtual CD-ROM.

The ISO root includes `THIRD_PARTY_NOTICES.md` for redistribution attribution.

## Dependencies

The builder shells out to an ISO authoring tool:

- Linux: `xorriso` (preferred) or `genisoimage` / `mkisofs`
- Windows (optional): `oscdimg` (from the Windows ADK)

## Build

From repo root:

```bash
python3 tools/driver-iso/build.py \
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

```bash
python3 tools/driver-iso/verify_iso.py \
  --iso dist/aero-virtio-win7-drivers-sample.iso
```
