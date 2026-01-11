# Guest Tools configuration

`devices.cmd` contains the device identifiers that the Guest Tools installer uses for:

- Boot-critical pre-seeding (`CriticalDeviceDatabase`) for `virtio-blk`
- Storage service name to mark as `BOOT_START`
- Expected PCI `VEN/DEV` pairs for Aero devices (virtio-blk/net/snd/input + Aero GPU)

This file is intended to be kept in sync with:

- The emulator's presented PCI IDs
- The `.inf` hardware ID matches
- The storage driver's service name (INF `AddService` name)

When building Guest Tools media from CI-produced signed driver packages, `ci/package-guest-tools.ps1`
may update the **staged** copy of `devices.cmd` to keep the storage service name
(`AERO_VIRTIO_BLK_SERVICE`) consistent with the packaged virtio-blk driver's INF `AddService` name.
This keeps boot-critical pre-seeding aligned with whichever storage driver is actually being shipped.

The `*_HWIDS` values are stored as a list of individually quoted hardware IDs to safely include
`&` characters (e.g. `"PCI\VEN_1AF4&DEV_1042&REV_01"`).

Contract v1 is **virtio-pci modern-only** (PCI Revision ID `0x01`). For virtio net/blk, the relevant
`VEN/DEV` pairs are:

- `PCI\VEN_1AF4&DEV_1041` (virtio-net)
- `PCI\VEN_1AF4&DEV_1042` (virtio-blk)

Some devices include multiple IDs (for example `REV`/`SUBSYS`-qualified variants) so Guest Tools can
recognize either enumeration.

For Aero virtio devices, these IDs are expected to follow the repo's device contract (virtio-pci
modern-only IDs plus PCI Revision ID `0x01`). Keep `devices.cmd` consistent with:

- `docs/windows7-virtio-driver-contract.md` (behavioral contract)
- `docs/windows-device-contract.json` (machine-readable manifest)

`AERO_VIRTIO_BLK_SERVICE` must match the virtio-blk storage driver's INF `AddService` name
(for Aero in-tree drivers: `aerovblk` from `drivers/windows7/virtio/blk/aerovblk.inf`).

## AeroGPU PCI IDs

The supported AeroGPU Win7 driver package binds to:

- `PCI\VEN_A3A0&DEV_0001` — current versioned ABI (canonical)

A legacy bring-up device model exists (`PCI\VEN_1AED&DEV_0001`), but the shipped AeroGPU INFs
intentionally do **not** bind to it. If you enable the legacy device model for bring-up, install
with a custom INF that matches `PCI\VEN_1AED&DEV_0001`.

`devices.cmd` lists only the canonical HWID in `AERO_GPU_HWIDS`. The older 1AE0-family vendor ID is
deprecated/stale and should not be used.
