# Guest Tools configuration

`devices.cmd` contains the device identifiers that the Guest Tools installer uses for:

- Boot-critical pre-seeding (`CriticalDeviceDatabase`) for `virtio-blk`
- Storage service name to mark as `BOOT_START`
- Expected PCI hardware IDs for Aero devices (virtio-blk/net/snd/input + Aero GPU)

`devices.cmd` is generated from the canonical machine-readable device contract:

- `docs/windows-device-contract.json`

The `*_HWIDS` values are stored as a list of individually quoted hardware IDs to safely include
`&` characters (e.g. `"PCI\VEN_1AF4&DEV_1042&REV_01"`).

Contract v1 (`AERO-W7-VIRTIO`) is virtio-pci **modern-only** with PCI Revision `0x01`, so Guest Tools expects
modern device IDs for net/blk:

- `PCI\VEN_1AF4&DEV_1041` (virtio-net, `REV_01`)
- `PCI\VEN_1AF4&DEV_1042` (virtio-blk, `REV_01`)

`AERO_VIRTIO_BLK_SERVICE` MUST match the virtio-blk storage driver's INF `AddService` name, because `setup.cmd`
uses it to mark the storage service as `BOOT_START` and to pre-seed `CriticalDeviceDatabase` entries.
(For Aero in-tree drivers, this is `aerovblk` from `drivers/windows7/virtio/blk/aerovblk.inf`.)
