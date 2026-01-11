# Guest Tools configuration

`devices.cmd` contains the device identifiers that the Guest Tools installer uses for:

- Boot-critical pre-seeding (`CriticalDeviceDatabase`) for `virtio-blk`
- Storage service name to mark as `BOOT_START`
- Expected PCI hardware IDs for Aero devices (virtio-blk/net/snd/input + Aero GPU)

`devices.cmd` is generated from the canonical machine-readable device contract:

- `docs/windows-device-contract.json`

Regenerate it with:

```bash
python3 scripts/generate-guest-tools-devices-cmd.py
```

The `*_HWIDS` values are stored as a list of individually quoted hardware IDs to safely include
`&` characters (e.g. `"PCI\VEN_1AF4&DEV_1042&REV_01"`).

Contract v1 (`AERO-W7-VIRTIO`) is virtio-pci **modern-only** with PCI Revision `0x01`, so Guest Tools expects
modern device IDs for all virtio devices:

- `PCI\VEN_1AF4&DEV_1041` (virtio-net, `REV_01`)
- `PCI\VEN_1AF4&DEV_1042` (virtio-blk, `REV_01`)
- `PCI\VEN_1AF4&DEV_1052` (virtio-input keyboard/mouse, `REV_01`)
- `PCI\VEN_1AF4&DEV_1059` (virtio-snd audio, `REV_01`)

Some devices include multiple IDs (for example `REV`/`SUBSYS`-qualified variants) so Guest Tools can
recognize either enumeration.

For Aero virtio devices, these IDs are expected to follow the repo's device contract (virtio-pci
modern-only IDs plus PCI Revision ID `0x01`). Keep `devices.cmd` consistent with:

- `docs/windows7-virtio-driver-contract.md` (behavioral contract)
- `docs/windows-device-contract.json` (machine-readable manifest)

`AERO_VIRTIO_BLK_SERVICE` MUST match the virtio-blk storage driver's INF `AddService` name, because `setup.cmd`
uses it to mark the storage service as `BOOT_START` and to pre-seed `CriticalDeviceDatabase` entries.
(For Aero in-tree drivers, this is `aerovblk` from `drivers/windows7/virtio/blk/aerovblk.inf`.)

## AeroGPU PCI IDs

The supported AeroGPU Windows driver stack binds to the canonical versioned ABI:

- `PCI\VEN_A3A0&DEV_0001`

The deprecated legacy bring-up ABI uses a different HWID and is intentionally not part of the
default Guest Tools config. If you need legacy bring-up, use the legacy AeroGPU INFs under
`drivers/aerogpu/packaging/win7/legacy/`, build the emulator with the legacy device model enabled
(feature `emulator/aerogpu-legacy`), and supply a custom `devices.cmd`.

The older 1AE0-family vendor ID is deprecated/stale and should not be used.
