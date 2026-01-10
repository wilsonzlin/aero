# Driver ISO builder

This tool builds an ISO image that can be mounted inside Aero as a virtual CD-ROM.

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

